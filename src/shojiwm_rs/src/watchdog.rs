//! Hang watchdog for a config that runs on the compositor thread.
//!
//! A Rust config answers the compositor in place, so a call into it that
//! never returns (an endless loop, a deadlock) freezes the whole session, and
//! nothing in the process can interrupt it. A watcher thread notices a call
//! that has stayed stuck past its budget and logs it. With [`OnHang::Abort`]
//! it then ends the process with SIGABRT aimed at the stuck thread: the
//! display manager comes back, and the core dump opens on the code that hung.
//!
//! Only time the config can be blamed for is counted. A look that finds the
//! thread in the kernel (disk, swap-in), stopped, paging in, or reclaiming
//! while the machine is short of memory adds nothing; a running thread is
//! charged only the CPU time it got; a watcher that was itself stopped
//! (suspend, SIGSTOP, a debugger) counts one look at most.

use std::{
    cell::RefCell,
    fmt,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, PoisonError, Weak,
        atomic::{
            AtomicBool, AtomicU64,
            Ordering::{Acquire, Release},
        },
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use shojiwm_lib::{
    runtime_api::{
        ConfigRuntime, DecorationRequest as D, EffectRequest as E, InputRequest as I,
        ReloadPreparation, RuntimeError, RuntimeEvent as Ev, RuntimeReply, RuntimeRequest as R,
        WindowRequest as W, WorkspaceRequest,
    },
    ssd::WaylandWindowSnapshot,
};

/// Logged once when a call has been charged this long.
const WARN_AFTER: Duration = Duration::from_secs(2);
/// Time the non-blocking log writer gets to drain before the signal.
const LOG_DRAIN: Duration = Duration::from_millis(300);
/// The most the report may wait on the disk (or a stderr pipe) before the
/// signal goes out anyway.
const REPORT_WAIT: Duration = Duration::from_secs(1);
/// Time SIGABRT gets to end the process before the watcher aborts itself.
const ABORT_FALLBACK: Duration = Duration::from_secs(5);
/// `/proc/pressure/memory` "full avg10" at or above this: the machine is
/// short of memory, and a slow call is not the config's fault.
const MEMORY_FULL_PRESSURE: f32 = 10.0;
/// `/proc/pressure/memory` "some avg10" at or above this while the thread
/// takes minor faults with little user time: it is reclaiming memory, not
/// running config code. "full" alone misses reclaim during a parallel
/// build, when other tasks keep running.
const MEMORY_SOME_PRESSURE: f32 = 20.0;
/// Nanoseconds per clock tick of `/proc/*/stat` times (USER_HZ is 100 on Linux).
const NS_PER_TICK: u64 = 10_000_000;
/// Appended to, one line per hang, in the report directory.
pub const MARKER_FILE: &str = "config-hangs.txt";

macro_rules! kinds {
    ($($(#[$attr:meta])* $kind:ident = $name:literal,)*) => {
        /// What the compositor thread is doing inside the config, named as the
        /// TypeScript runtime names its requests so the logs read the same.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u8)]
        enum Kind { $($(#[$attr])* $kind,)* }
        const NAMES: &[&str] = &[$($name,)*];
    };
}

kinds! {
    /// Only ever the 0 in `Shared::busy`.
    #[allow(dead_code)]
    Idle = "idle",
    // Lifecycle calls, up to and including `Shutdown`: the lifecycle budget.
    Launch = "launch",
    Preload = "preload",
    Enable = "enable",
    PrepareReload = "prepareReload",
    Reload = "reload",
    Shutdown = "shutdown",
    Evaluate = "evaluate",
    EvaluatePreview = "evaluatePreview",
    EvaluateCached = "evaluateCached",
    Policy = "windowDecorationPolicy",
    InvokeHandler = "invokeHandler",
    StartClose = "startClose",
    WindowClosed = "windowClosed",
    SchedulerTick = "schedulerTick",
    WindowResize = "windowResize",
    WindowMove = "windowMove",
    MaximizeRequest = "windowMaximizeRequest",
    MinimizeRequest = "windowMinimizeRequest",
    FullscreenRequest = "windowFullscreenRequest",
    ActivateRequest = "windowActivateRequest",
    KeyBinding = "invokeKeyBinding",
    PointerMove = "pointerMove",
    GestureSwipe = "gestureSwipe",
    BackgroundEffect = "getEffectConfig",
    LayerEffects = "evaluateLayerEffects",
    PopupEffects = "evaluatePopupEffects",
    WorkspaceActivate = "workspaceActivate",
    DisplayState = "displayState",
    InputState = "inputState",
    KeyboardLayout = "keyboardLayout",
    PointerMoveAsync = "pointerMoveAsync",
    GestureSwipeAsync = "gestureSwipeAsync",
}

fn kind_name(code: u8) -> &'static str {
    NAMES.get(usize::from(code)).copied().unwrap_or("unknown")
}

fn is_lifecycle(code: u8) -> bool {
    code != Kind::Idle as u8 && code <= Kind::Shutdown as u8
}

/// What to do once a call has used up its budget.
#[derive(Clone)]
pub enum OnHang {
    /// Report, then end the process with SIGABRT aimed at the stuck thread.
    /// Only logs while a debugger is attached.
    Abort,
    /// Log the report and keep waiting.
    Log,
    /// Hand the report to a callback instead (tests, embedders).
    Call(Arc<dyn Fn(&HangReport) + Send + Sync>),
}

/// Watches every call into an in-process config.
/// [`ConfigBuilder`](crate::ConfigBuilder) and [`RustLauncher`](crate::RustLauncher)
/// start one that only logs ([`log_only`](Self::log_only)); a config that
/// would rather end a frozen session passes `HangWatchdog::default()`, which
/// aborts.
#[derive(Clone)]
pub struct HangWatchdog {
    call_budget: Duration,
    lifecycle_budget: Duration,
    poll: Duration,
    on_hang: OnHang,
    report_dir: Option<PathBuf>,
}

impl Default for HangWatchdog {
    fn default() -> Self {
        Self {
            call_budget: Duration::from_secs(10),
            lifecycle_budget: Duration::from_secs(30),
            poll: Duration::from_secs(1),
            on_hang: OnHang::Abort,
            report_dir: None,
        }
    }
}

impl HangWatchdog {
    /// Detect and log, never end the process.
    pub fn log_only() -> Self {
        Self::default().on_hang(OnHang::Log)
    }

    /// Stuck time allowed in one request or event (default 10 s).
    pub fn call_budget(mut self, budget: Duration) -> Self {
        self.call_budget = budget;
        self
    }

    /// Stuck time allowed in launch, preload, enable, prepare_reload,
    /// reload and shutdown (default 30 s).
    pub fn lifecycle_budget(mut self, budget: Duration) -> Self {
        self.lifecycle_budget = budget;
        self
    }

    pub fn on_hang(mut self, on_hang: OnHang) -> Self {
        self.on_hang = on_hang;
        self
    }

    /// How often the watcher looks (default 1 s; held to 5 ms - 60 s). One
    /// look counts for at most twice this, so a stopped process never adds
    /// up.
    pub fn poll(mut self, poll: Duration) -> Self {
        // A zero poll would spin and never count, a huge one never look.
        self.poll = poll.clamp(Duration::from_millis(5), Duration::from_secs(60));
        self
    }

    /// Where [`MARKER_FILE`] is appended to (default `~/shoji_wm/logs`, next
    /// to the session logs).
    pub fn report_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.report_dir = Some(dir.into());
        self
    }

    fn budget(&self, code: u8) -> Duration {
        if is_lifecycle(code) {
            self.lifecycle_budget
        } else {
            self.call_budget
        }
    }
}

/// `SHOJI_RUNTIME_WATCHDOG=off|0|false`: only log. The environment can turn
/// the watchdog down, never on, so a session it never reaches stays safe.
fn env_says_off(value: Option<&str>) -> bool {
    matches!(value.map(str::trim), Some("off" | "0" | "false"))
}

/// A call that used up its budget.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HangReport {
    /// `invokeKeyBinding`, `schedulerTick`, ...
    pub kind: &'static str,
    /// Binding id, handler id, window id or app id, for discrete calls.
    pub label: Option<String>,
    /// Time charged to the config.
    pub counted: Duration,
    /// Time since the watcher first saw the call, held looks included.
    pub in_flight: Duration,
    /// Kernel id of the stuck thread (the compositor thread).
    pub tid: i32,
    /// Looks that found it running, asleep, or held (not charged).
    pub running: u32,
    pub blocked: u32,
    pub held: u32,
}

impl fmt::Display for HangReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = self
            .label
            .as_deref()
            .map(|label| format!(" [{label}]"))
            .unwrap_or_default();
        let why = if self.running >= self.blocked {
            "running: probably an endless loop"
        } else {
            "asleep: probably a deadlock or a wait that never ends"
        };
        write!(
            f,
            "config call `{}`{label} has not returned: {:.1?} counted ({:.1?} in flight); \
             thread {} was {why} (R {} / S {} / held {} looks)",
            self.kind,
            self.counted,
            self.in_flight,
            self.tid,
            self.running,
            self.blocked,
            self.held
        )
    }
}

struct Shared {
    /// 0 while idle, else `seq << 8 | kind` of the call in flight.
    busy: AtomicU64,
    /// Label of the call with this seq; stale once `busy` moves on.
    label: Mutex<(u64, String)>,
    stop: AtomicBool,
}

/// The watcher thread and the stamps the compositor thread writes for it.
pub(crate) struct Armed {
    shared: Arc<Shared>,
    seq: u64,
    watcher: Option<thread::JoinHandle<()>>,
}

impl Armed {
    /// On the thread that makes every call into the config: the compositor
    /// thread. A `Box<dyn ConfigRuntime>` is not `Send`, so a runtime is
    /// always called on the thread that launched it.
    pub(crate) fn start(mut config: HangWatchdog) -> Self {
        let off = env_says_off(std::env::var("SHOJI_RUNTIME_WATCHDOG").ok().as_deref());
        if off && matches!(config.on_hang, OnHang::Abort) {
            config.on_hang = OnHang::Log;
        }
        // Resolved here: the watcher never reads the environment, which
        // other threads may be changing.
        if config.report_dir.is_none() {
            config.report_dir =
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join("shoji_wm/logs"));
        }
        // SAFETY: gettid has no preconditions.
        let tid = unsafe { libc::gettid() };
        let action = match config.on_hang {
            OnHang::Abort => "abort",
            OnHang::Log if off => "log only (SHOJI_RUNTIME_WATCHDOG=off)",
            OnHang::Log => "log only",
            OnHang::Call(_) => "callback",
        };
        tracing::info!(
            call = ?config.call_budget,
            lifecycle = ?config.lifecycle_budget,
            action,
            "config hang watchdog armed"
        );
        let shared = Arc::new(Shared {
            busy: AtomicU64::new(0),
            label: Mutex::default(),
            stop: AtomicBool::new(false),
        });
        if let (OnHang::Abort, Some(dir)) = (&config.on_hang, &config.report_dir) {
            for line in PREVIOUS.get_or_init(|| take_unseen(dir)) {
                tracing::warn!("the previous session was ended by the config watchdog: {line}");
            }
        }
        CURRENT.with(|current| *current.borrow_mut() = Arc::downgrade(&shared));
        let watched = Arc::clone(&shared);
        let watcher = thread::Builder::new()
            .name("shoji-cfg-wdog".into())
            .spawn(move || watch(&watched, &config, tid))
            .map_err(|error| tracing::warn!(%error, "config hang watchdog not started"))
            .ok();
        Self {
            shared,
            seq: 0,
            watcher,
        }
    }

    /// Two atomic stores around the call; a short lock only for labelled
    /// (discrete) calls.
    fn timed<T>(&mut self, kind: Kind, label: Option<&str>, call: impl FnOnce() -> T) -> T {
        self.seq += 1;
        if let Some(label) = label {
            let mut slot = self
                .shared
                .label
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            slot.0 = self.seq;
            slot.1.clear();
            slot.1.push_str(label);
        }
        // Release: a watcher that sees this seq also sees its label.
        self.shared.busy.store(self.seq << 8 | kind as u64, Release);
        let _idle = Idle(&self.shared.busy);
        call()
    }
}

/// Clears `busy` when the call returns, and when it unwinds.
struct Idle<'a>(&'a AtomicU64);

impl Drop for Idle<'_> {
    fn drop(&mut self) {
        self.0.store(0, Release);
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        CURRENT.with(|current| {
            let mut current = current.borrow_mut();
            if std::ptr::eq(current.as_ptr(), Arc::as_ptr(&self.shared)) {
                *current = Weak::new();
            }
        });
        self.shared.stop.store(true, Release);
        if let Some(watcher) = self.watcher.take() {
            watcher.thread().unpark();
            let _ = watcher.join();
        }
    }
}

thread_local! {
    /// The watchdog of the runtime this thread calls into.
    static CURRENT: RefCell<Weak<Shared>> = const { RefCell::new(Weak::new()) };
}

/// Until dropped, a hang is reported with `what` as its label; then the
/// call's own label is back. For config code that runs inside whatever
/// call drained it: IPC handlers, channel drains.
pub(crate) fn note(what: &str) -> Option<Noted> {
    let shared = CURRENT.with(|current| current.borrow().upgrade())?;
    let seq = shared.busy.load(Acquire) >> 8;
    if seq == 0 {
        return None;
    }
    let before = {
        let mut slot = shared.label.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::replace(&mut *slot, (seq, what.to_owned()))
    };
    Some(Noted { shared, before })
}

pub(crate) struct Noted {
    shared: Arc<Shared>,
    before: (u64, String),
}

impl Drop for Noted {
    fn drop(&mut self) {
        let mut slot = self.shared.label.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = std::mem::take(&mut self.before);
    }
}

/// A runtime whose every call is watched.
pub(crate) struct Watched<C> {
    inner: C,
    // After `inner`: the watcher outlives the runtime's own drop.
    armed: Armed,
}

/// Build the runtime under the watchdog: its constructor is config code too.
/// `None` builds it bare, with no thread.
pub(crate) fn launch<C: ConfigRuntime + 'static>(
    watchdog: Option<HangWatchdog>,
    start: impl FnOnce() -> C,
) -> Box<dyn ConfigRuntime> {
    match watchdog {
        None => Box::new(start()),
        Some(config) => {
            let mut armed = Armed::start(config);
            let inner = armed.timed(Kind::Launch, None, start);
            Box::new(Watched { inner, armed })
        }
    }
}

impl<C> Watched<C> {
    fn call<T>(&mut self, kind: Kind, label: Option<&str>, call: impl FnOnce(&mut C) -> T) -> T {
        let Self { inner, armed } = self;
        armed.timed(kind, label, || call(inner))
    }
}

impl<C: ConfigRuntime> ConfigRuntime for Watched<C> {
    fn preload(&mut self) -> Result<(), RuntimeError> {
        self.call(Kind::Preload, None, C::preload)
    }

    fn enable(&mut self) -> Result<(), RuntimeError> {
        self.call(Kind::Enable, None, C::enable)
    }

    fn prepare_reload(&mut self) -> Result<ReloadPreparation, RuntimeError> {
        self.call(Kind::PrepareReload, None, C::prepare_reload)
    }

    fn reload(&mut self) -> Result<(), RuntimeError> {
        self.call(Kind::Reload, None, C::reload)
    }

    fn shutdown(&mut self) {
        self.call(Kind::Shutdown, None, C::shutdown)
    }

    fn request(&mut self, now_ms: f64, request: R<'_>) -> Result<RuntimeReply, RuntimeError> {
        let (kind, label) = describe(request);
        self.call(kind, label, |inner| inner.request(now_ms, request))
    }

    fn post(&mut self, now_ms: f64, event: Ev) {
        let kind = match &event {
            Ev::DisplayState(_) => Kind::DisplayState,
            Ev::InputState(_) => Kind::InputState,
            Ev::KeyboardLayout(_) => Kind::KeyboardLayout,
            Ev::PointerMove(_) => Kind::PointerMoveAsync,
            Ev::GestureSwipe(_) => Kind::GestureSwipeAsync,
        };
        self.call(kind, None, |inner| inner.post(now_ms, event))
    }
}

/// Kind and label of a request. Per-frame requests carry no label, so they
/// cost two atomic stores and nothing else. No wildcard: a new request
/// variant does not compile until it is named here.
fn describe(request: R<'_>) -> (Kind, Option<&str>) {
    fn app(window: &WaylandWindowSnapshot) -> &str {
        window.app_id.as_deref().unwrap_or(&window.id)
    }
    match request {
        R::Decoration(D::Evaluate {
            window,
            preview: false,
        }) => (Kind::Evaluate, Some(app(window))),
        R::Decoration(D::Evaluate {
            window,
            preview: true,
        }) => (Kind::EvaluatePreview, Some(app(window))),
        R::Decoration(D::EvaluateCached { .. }) => (Kind::EvaluateCached, None),
        R::Decoration(D::Policy { window, .. }) => (Kind::Policy, Some(app(window))),
        R::Decoration(D::InvokeHandler { handler_id, .. }) => {
            (Kind::InvokeHandler, Some(handler_id))
        }
        R::Decoration(D::StartClose { window_id }) => (Kind::StartClose, Some(window_id)),
        R::Decoration(D::Closed { window_id }) => (Kind::WindowClosed, Some(window_id)),
        R::SchedulerTick => (Kind::SchedulerTick, None),
        R::Window(W::Resize { .. }) => (Kind::WindowResize, None),
        R::Window(W::Move { .. }) => (Kind::WindowMove, None),
        R::Window(W::Maximize { window, .. }) => (Kind::MaximizeRequest, Some(app(window))),
        R::Window(W::Minimize { window, .. }) => (Kind::MinimizeRequest, Some(app(window))),
        R::Window(W::Fullscreen { window, .. }) => (Kind::FullscreenRequest, Some(app(window))),
        R::Window(W::Activate { window, .. }) => (Kind::ActivateRequest, Some(app(window))),
        R::Input(I::KeyBinding { binding_id }) => (Kind::KeyBinding, Some(binding_id)),
        R::Input(I::PointerMove(_)) => (Kind::PointerMove, None),
        R::Input(I::GestureSwipe(_)) => (Kind::GestureSwipe, None),
        R::Effect(E::Background) => (Kind::BackgroundEffect, None),
        R::Effect(E::Layers { .. }) => (Kind::LayerEffects, None),
        R::Effect(E::Popups { .. }) => (Kind::PopupEffects, None),
        R::Workspace(WorkspaceRequest::Activate(_)) => (Kind::WorkspaceActivate, None),
    }
}

// ---- detection ------------------------------------------------------------

/// One look at the compositor thread.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Sample {
    /// `R`, `S`, `D`, `t`, ...
    state: u8,
    minflt: u64,
    majflt: u64,
    /// User CPU time, in clock ticks.
    utime: u64,
    /// Time on a CPU, and runnable but waiting for one, when the kernel
    /// says (`schedstat`).
    cpu_ns: Option<u64>,
    run_delay_ns: Option<u64>,
    /// Machine-wide, from `/proc/pressure/memory`; 0 when unknown.
    memory_some_avg10: f32,
    memory_full_avg10: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Look {
    /// In the config and runnable: a loop. Charged the CPU time it got, so
    /// a loop starved by a busy machine still adds up, at the rate it runs.
    Running(Duration),
    /// In an interruptible wait: a deadlock, or a wait that never ends.
    Blocked,
    /// Time that is not the config's, and why.
    Held(&'static str),
}

fn classify(prev: &Sample, now: &Sample, gap: Duration) -> Look {
    let half_gap_ns = u64::try_from(gap.as_nanos() / 2).unwrap_or(u64::MAX);
    let short_of_memory = now.memory_full_avg10 >= MEMORY_FULL_PRESSURE;
    match now.state {
        b'D' => Look::Held("in the kernel (disk or swap)"),
        b't' | b'T' => Look::Held("stopped"),
        _ if now.majflt > prev.majflt => Look::Held("paging in"),
        b'R' => {
            let delta = |now: Option<u64>, prev: Option<u64>| {
                now.zip(prev).map(|(now, prev)| now.saturating_sub(prev))
            };
            let user_ns = now
                .utime
                .saturating_sub(prev.utime)
                .saturating_mul(NS_PER_TICK);
            let reclaiming = now.minflt > prev.minflt && now.memory_some_avg10 >= MEMORY_SOME_PRESSURE;
            if user_ns < half_gap_ns && (short_of_memory || reclaiming) {
                // Kernel time under memory pressure is reclaim, not config code.
                return Look::Held("reclaiming memory");
            }
            match delta(now.cpu_ns, prev.cpu_ns) {
                Some(cpu_ns) => {
                    let ran = Duration::from_nanos(cpu_ns).min(gap);
                    if ran < gap / 10 {
                        Look::Held("waiting for a CPU")
                    } else {
                        Look::Running(ran)
                    }
                }
                // No schedstat: all or nothing on the time spent waiting.
                None => {
                    if delta(now.run_delay_ns, prev.run_delay_ns).is_some_and(|waited| waited > half_gap_ns) {
                        Look::Held("waiting for a CPU")
                    } else {
                        Look::Running(gap)
                    }
                }
            }
        }
        _ if short_of_memory => Look::Held("short of memory"),
        _ => Look::Blocked,
    }
}

/// Something worth saying after a look.
#[derive(Debug, PartialEq)]
enum Note {
    /// A call that had been charged at least `WARN_AFTER` returned.
    Returned {
        kind: u8,
        counted: Duration,
        in_flight: Duration,
    },
    /// The call in flight has been charged `WARN_AFTER`.
    Slow,
    /// The call has been in flight a whole budget, but the time was held.
    Held(&'static str),
    /// The call has used up its budget.
    Hung,
}

/// Per-call accounting, free of threads and clocks so every case is a test.
#[derive(Default)]
struct Tracker {
    /// `busy` of the call being followed; 0 while idle.
    seen: u64,
    prev: Option<Sample>,
    counted: Duration,
    in_flight: Duration,
    running: u32,
    blocked: u32,
    held: u32,
    held_why: Option<&'static str>,
    warned: bool,
    held_warned: bool,
    fired: bool,
}

impl Tracker {
    fn look(
        &mut self,
        busy: u64,
        elapsed: Duration,
        poll: Duration,
        budget: impl Fn(u8) -> Duration,
        read: impl FnOnce() -> Option<Sample>,
    ) -> Option<Note> {
        if busy != self.seen {
            let note = (self.counted >= WARN_AFTER).then_some(Note::Returned {
                kind: self.seen as u8,
                counted: self.counted,
                in_flight: self.in_flight,
            });
            *self = Self {
                seen: busy,
                ..Self::default()
            };
            if busy != 0 {
                self.prev = read(); // baseline: charging starts at the next look
            }
            return note;
        }
        if busy == 0 {
            return None;
        }
        // A watcher that was itself stopped (suspend, SIGSTOP, a debugger,
        // the whole process starved) counts one look at most, whatever the
        // clock did meanwhile.
        let gap = elapsed.min(poll.saturating_mul(2));
        self.in_flight += gap;
        let now = read();
        let look = match (self.prev, now) {
            (Some(prev), Some(now)) => classify(&prev, &now, gap),
            _ => Look::Blocked, // no /proc: still in flight, so count it
        };
        self.prev = now;
        match look {
            Look::Running(ran) => (self.running, self.counted) = (self.running + 1, self.counted + ran),
            Look::Blocked => (self.blocked, self.counted) = (self.blocked + 1, self.counted + gap),
            Look::Held(why) => (self.held, self.held_why) = (self.held + 1, Some(why)),
        }
        if self.fired {
            return None;
        }
        let budget = budget(busy as u8);
        if self.counted >= budget {
            (self.fired, self.warned) = (true, true);
            Some(Note::Hung)
        } else if !self.warned && self.counted >= WARN_AFTER {
            self.warned = true;
            Some(Note::Slow)
        } else if !self.held_warned && self.in_flight >= budget {
            self.held_warned = true;
            Some(Note::Held(self.held_why.unwrap_or("held")))
        } else {
            None
        }
    }

    fn report(&self, label: Option<String>, tid: i32) -> HangReport {
        HangReport {
            kind: kind_name(self.seen as u8),
            label,
            counted: self.counted,
            in_flight: self.in_flight,
            tid,
            running: self.running,
            blocked: self.blocked,
            held: self.held,
        }
    }
}

fn watch(shared: &Shared, config: &HangWatchdog, tid: i32) {
    let mut tracker = Tracker::default();
    let mut looked = Instant::now();
    while !shared.stop.load(Acquire) {
        thread::park_timeout(config.poll);
        let elapsed = looked.elapsed();
        looked = Instant::now();
        let busy = shared.busy.load(Acquire);
        let kind = kind_name(busy as u8);
        match tracker.look(
            busy,
            elapsed,
            config.poll,
            |code| config.budget(code),
            || read_sample(tid),
        ) {
            None => {}
            Some(Note::Returned {
                kind: returned,
                counted,
                in_flight,
            }) => tracing::warn!(
                kind = kind_name(returned),
                ?counted,
                ?in_flight,
                "slow config call returned"
            ),
            Some(Note::Slow) => {
                tracing::warn!(kind, counted = ?tracker.counted, "config call is taking long")
            }
            Some(Note::Held(why)) => tracing::warn!(
                kind,
                in_flight = ?tracker.in_flight,
                counted = ?tracker.counted,
                "config call is stuck, but not charged: thread {why}"
            ),
            Some(Note::Hung) => {
                let label = {
                    let slot = shared.label.lock().unwrap_or_else(PoisonError::into_inner);
                    (slot.0 == busy >> 8).then(|| slot.1.clone())
                };
                fire(shared, config, &tracker.report(label, tid), busy);
            }
        }
    }
}

/// Byte length of the marker that the last session had already seen.
const SEEN_FILE: &str = "config-hangs.seen";
static PREVIOUS: OnceLock<Vec<String>> = OnceLock::new();

/// Hangs the watchdog reported since the last session started (the last
/// five at most), for a config that wants to show them, e.g. from
/// `on_enable`. Empty until a watchdog is armed, and when it only logs.
pub fn previous_hangs() -> &'static [String] {
    PREVIOUS.get().map_or(&[], Vec::as_slice)
}

/// Abort lines appended since the last session looked, the last five at
/// most. An abort the watchdog stood down from (the call returned while it
/// was reported) ended nothing and is left out. The seen file holds how far
/// the last session read and the line it read up to, so a marker deleted or
/// rewritten in between is read from the start rather than from a stale
/// offset.
fn take_unseen(dir: &Path) -> Vec<String> {
    let Ok(marker) = std::fs::read_to_string(dir.join(MARKER_FILE)) else {
        return Vec::new();
    };
    // Whole lines only: a line still being written is read next time.
    let complete = marker.rfind('\n').map_or(0, |end| end + 1);
    let sidecar = std::fs::read_to_string(dir.join(SEEN_FILE)).unwrap_or_default();
    let (seen, last_seen) = sidecar.split_once('\n').unwrap_or((sidecar.as_str(), ""));
    let seen = seen
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|&seen| {
            seen <= complete
                && marker.is_char_boundary(seen)
                && marker[..seen].ends_with(&format!("{last_seen}\n"))
        })
        .unwrap_or(0);
    let last_line = marker[..complete].lines().last().unwrap_or_default();
    let _ = std::fs::write(dir.join(SEEN_FILE), format!("{complete}\n{last_line}"));
    let unseen = &marker[seen..complete];
    let stood_down: std::collections::HashSet<&str> = unseen
        .lines()
        .filter_map(|line| line.split_once(" stood down from ")?.1.split_once(": "))
        .map(|(key, _)| key)
        .collect();
    let aborts: Vec<String> = unseen
        .lines()
        .filter(|line| {
            line.split_once(" abort: ")
                .is_some_and(|(key, _)| !stood_down.contains(key))
        })
        .map(str::to_owned)
        .collect();
    aborts[aborts.len().saturating_sub(5)..].to_vec()
}

fn read_sample(tid: i32) -> Option<Sample> {
    let task = format!("/proc/self/task/{tid}");
    let (state, minflt, majflt, utime) =
        parse_stat(&std::fs::read_to_string(format!("{task}/stat")).ok()?)?;
    let (cpu_ns, run_delay_ns) = std::fs::read_to_string(format!("{task}/schedstat"))
        .ok()
        .and_then(|schedstat| parse_schedstat(&schedstat))
        .unzip();
    let psi = std::fs::read_to_string("/proc/pressure/memory").unwrap_or_default();
    Some(Sample {
        state,
        minflt,
        majflt,
        utime,
        cpu_ns,
        run_delay_ns,
        memory_some_avg10: parse_avg10(&psi, "some").unwrap_or(0.0),
        memory_full_avg10: parse_avg10(&psi, "full").unwrap_or(0.0),
    })
}

/// State (field 3), minflt (10), majflt (12) and utime (14) of a `stat`
/// line, counted after the command name, which may contain spaces and
/// parentheses.
fn parse_stat(stat: &str) -> Option<(u8, u64, u64, u64)> {
    let mut fields = stat.get(stat.rfind(')')? + 1..)?.split_ascii_whitespace();
    let state = *fields.next()?.as_bytes().first()?;
    let minflt = fields.nth(6)?.parse().ok()?;
    let majflt = fields.nth(1)?.parse().ok()?;
    let utime = fields.nth(1)?.parse().ok()?;
    Some((state, minflt, majflt, utime))
}

/// `schedstat` is "cpu_ns run_delay_ns timeslices".
fn parse_schedstat(schedstat: &str) -> Option<(u64, u64)> {
    let mut fields = schedstat.split_ascii_whitespace();
    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
}

/// "avg10" of the `some` or `full` line of a PSI file.
fn parse_avg10(psi: &str, line: &str) -> Option<f32> {
    psi.lines()
        .find_map(|current| current.strip_prefix(line)?.strip_prefix(' '))?
        .split_ascii_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))?
        .parse()
        .ok()
}

fn parse_traced(status: &str) -> bool {
    status
        .lines()
        .find_map(|line| line.strip_prefix("TracerPid:"))
        .is_some_and(|pid| pid.trim() != "0")
}

// ---- action ---------------------------------------------------------------

fn fire(shared: &Shared, config: &HangWatchdog, report: &HangReport, busy: u64) {
    match &config.on_hang {
        OnHang::Call(hook) => hook(report),
        OnHang::Log => tracing::error!("config hang (the watchdog only logs): {report}"),
        OnHang::Abort
            if std::fs::read_to_string("/proc/self/status").is_ok_and(|s| parse_traced(&s)) =>
        {
            tracing::error!("config hang; a debugger is attached, so not aborting: {report}")
        }
        OnHang::Abort => abort_session(shared, config.report_dir.as_deref(), report, busy),
    }
}

/// Every step before the signal is bounded: the report never keeps the
/// session frozen.
fn abort_session(shared: &Shared, dir: Option<&Path>, report: &HangReport, busy: u64) {
    let pid = std::process::id();
    // Names this report in the lines that follow it.
    let key = format!("{} pid {pid}", stamp());
    report_bounded(
        dir,
        format!("{key} abort: {report}. Stack: coredumpctl info {pid}"),
        true,
    );
    thread::sleep(LOG_DRAIN);
    let returned = || shared.busy.load(Acquire) != busy;
    if !returned() {
        // Client wl_shm pools are anonymous shared memory: leave them out of
        // the core, which is then smaller, quicker, and holds no app pixels.
        let _ = std::fs::write("/proc/self/coredump_filter", "0x31");
        raise_core_limit();
    }
    if returned() {
        let line = format!(
            "{} stood down from {key}: the call returned while it was being reported",
            stamp()
        );
        report_bounded(dir, line, false);
        return;
    }
    // SAFETY: tgkill only sends a signal; `tid` is the compositor thread,
    // alive because it is the one stuck. Taking SIGABRT makes it the
    // dumping thread: the core, coredumpctl and gdb open on the hung frame.
    unsafe { libc::syscall(libc::SYS_tgkill, libc::getpid(), report.tid, libc::SIGABRT) };
    thread::sleep(ABORT_FALLBACK);
    // Not taken: SIGABRT blocked or handled on that thread, or it is stuck
    // in the kernel. abort() unblocks and raises it here, so the core then
    // opens on this thread rather than on the hung frame: say so.
    let line = format!(
        "{} fallback from {key}: SIGABRT was not taken in {ABORT_FALLBACK:?}, aborting from the watcher",
        stamp()
    );
    report_bounded(dir, line, true);
    std::process::abort();
}

/// Log, append to the marker and write to stderr on a helper thread, waiting
/// for it `REPORT_WAIT` at most: the disk may be why everything is slow.
fn report_bounded(dir: Option<&Path>, line: String, error: bool) {
    let marker = dir.map(|dir| dir.join(MARKER_FILE));
    let (done, finished) = mpsc::channel::<()>();
    let fallback = line.clone();
    let helper = thread::Builder::new()
        .name("shoji-cfg-rept".into())
        .spawn(move || {
            if error {
                tracing::error!("{line}; ending the session so it does not stay frozen");
            } else {
                tracing::warn!("{line}");
            }
            if let Some(marker) = marker {
                let _ = append_line(&marker, &line);
            }
            let _ = writeln!(std::io::stderr(), "shoji_wm: {line}");
            let _ = done.send(());
        });
    match helper {
        Ok(_) => {
            let _ = finished.recv_timeout(REPORT_WAIT);
        }
        // No thread to spare: the log writer never blocks, so log inline.
        Err(_) => tracing::error!("{fallback}"),
    }
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(format!("{line}\n").as_bytes())?;
    file.sync_data()
}

/// The session's core limit may be soft 0 (systemd's default for many
/// sessions): raise it to the hard limit for this deliberate dump.
fn raise_core_limit() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: both calls get a valid pointer to an rlimit.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_CORE, &mut limit) == 0 && limit.rlim_cur < limit.rlim_max {
            limit.rlim_cur = limit.rlim_max;
            libc::setrlimit(libc::RLIMIT_CORE, &limit);
        }
    }
}

/// `@<epoch> <UTC>`: the epoch is exact (`date -d @<epoch>` prints local
/// time), and UTC matches the session logs.
fn stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    format!("@{secs} {}", utc(secs))
}

fn utc(secs: u64) -> String {
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use std::{os::unix::process::ExitStatusExt, process::Command};

    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn busy(seq: u64, kind: Kind) -> u64 {
        seq << 8 | kind as u64
    }

    fn sample(state: u8) -> Sample {
        Sample {
            state,
            run_delay_ns: Some(0),
            ..Sample::default()
        }
    }

    fn budgets(code: u8) -> Duration {
        HangWatchdog::default().budget(code)
    }

    // -- pure ---------------------------------------------------------------

    #[test]
    fn only_the_configs_own_time_is_counted() {
        let gap = Duration::from_secs(1);
        let base = sample(b'R');
        assert_eq!(classify(&base, &sample(b'R'), gap), Look::Running(gap));
        assert_eq!(classify(&base, &sample(b'S'), gap), Look::Blocked);
        assert!(matches!(classify(&base, &sample(b'D'), gap), Look::Held(_)));
        assert!(matches!(classify(&base, &sample(b't'), gap), Look::Held(_)));
        assert!(matches!(classify(&base, &sample(b'T'), gap), Look::Held(_)));
        let paging = Sample {
            majflt: 3,
            ..sample(b'R')
        };
        assert!(matches!(
            classify(&base, &paging, gap),
            Look::Held("paging in")
        ));
        let starved = Sample {
            run_delay_ns: Some(600_000_000),
            ..sample(b'R')
        };
        assert!(matches!(
            classify(&base, &starved, gap),
            Look::Held("waiting for a CPU")
        ));
        let unknown_delay = Sample {
            run_delay_ns: None,
            ..sample(b'R')
        };
        assert_eq!(classify(&base, &unknown_delay, gap), Look::Running(gap));
        // Short of memory: only user time is the config's.
        let reclaim = Sample {
            memory_full_avg10: 25.0,
            utime: 10,
            ..sample(b'R')
        };
        assert!(matches!(
            classify(&base, &reclaim, gap),
            Look::Held("reclaiming memory")
        ));
        let spinning = Sample {
            memory_full_avg10: 25.0,
            utime: 90,
            ..sample(b'R')
        };
        assert_eq!(classify(&base, &spinning, gap), Look::Running(gap));
        // With schedstat, a runnable look is charged the CPU time it got: a
        // loop on an oversubscribed machine still adds up, slower.
        let on_cpu = |ns: u64| Sample {
            cpu_ns: Some(ns),
            ..sample(b'R')
        };
        let base_cpu = on_cpu(0);
        assert_eq!(classify(&base_cpu, &on_cpu(1_000_000_000), gap), Look::Running(gap));
        assert_eq!(
            classify(&base_cpu, &on_cpu(300_000_000), gap),
            Look::Running(Duration::from_millis(300))
        );
        assert!(matches!(
            classify(&base_cpu, &on_cpu(50_000_000), gap),
            Look::Held("waiting for a CPU")
        ));
        // Reclaim during a build: "some" pressure, minor faults, little
        // user time. A loop that allocates spends its time in user code.
        let reclaiming = |utime: u64| Sample {
            minflt: 500,
            utime,
            memory_some_avg10: 40.0,
            ..on_cpu(900_000_000)
        };
        assert!(matches!(
            classify(&base_cpu, &reclaiming(5), gap),
            Look::Held("reclaiming memory")
        ));
        assert_eq!(
            classify(&base_cpu, &reclaiming(90), gap),
            Look::Running(Duration::from_millis(900))
        );
        let no_faults = Sample {
            minflt: 0,
            ..reclaiming(5)
        };
        assert_eq!(
            classify(&base_cpu, &no_faults, gap),
            Look::Running(Duration::from_millis(900))
        );
        let waiting = Sample {
            memory_full_avg10: 25.0,
            ..sample(b'S')
        };
        assert!(matches!(
            classify(&base, &waiting, gap),
            Look::Held("short of memory")
        ));
    }

    #[test]
    fn a_stopped_watcher_counts_one_look_at_most() {
        let mut tracker = Tracker::default();
        let call = busy(1, Kind::KeyBinding);
        let poll = Duration::from_secs(1);
        assert_eq!(
            tracker.look(call, poll, poll, budgets, || Some(sample(b'R'))),
            None
        );
        // Ten minutes asleep (suspend, SIGSTOP, a breakpoint) between looks.
        tracker.look(call, Duration::from_secs(600), poll, budgets, || {
            Some(sample(b'R'))
        });
        assert_eq!(tracker.counted, poll * 2);
    }

    #[test]
    fn a_call_fires_once_at_its_budget_and_a_new_call_starts_over() {
        let mut tracker = Tracker::default();
        let poll = Duration::from_secs(1);
        let first = busy(1, Kind::SchedulerTick);
        let mut notes = Vec::new();
        for _ in 0..15 {
            notes.extend(tracker.look(first, poll, poll, budgets, || Some(sample(b'S'))));
        }
        assert_eq!(notes, [Note::Slow, Note::Hung]);
        assert_eq!(tracker.counted, Duration::from_secs(14));
        let second = busy(2, Kind::SchedulerTick);
        let note = tracker.look(second, poll, poll, budgets, || Some(sample(b'S')));
        assert!(matches!(note, Some(Note::Returned { .. })));
        assert_eq!((tracker.counted, tracker.fired), (Duration::ZERO, false));
    }

    #[test]
    fn lifecycle_calls_get_the_lifecycle_budget() {
        for kind in [
            Kind::Launch,
            Kind::Preload,
            Kind::Enable,
            Kind::PrepareReload,
            Kind::Reload,
            Kind::Shutdown,
        ] {
            assert!(is_lifecycle(kind as u8), "{kind:?}");
            assert_eq!(budgets(kind as u8), Duration::from_secs(30));
        }
        for kind in [
            Kind::Idle,
            Kind::Evaluate,
            Kind::KeyBinding,
            Kind::GestureSwipeAsync,
        ] {
            assert!(!is_lifecycle(kind as u8), "{kind:?}");
        }
        assert_eq!(budgets(Kind::SchedulerTick as u8), Duration::from_secs(10));
    }

    #[test]
    fn held_time_never_fires_but_is_reported_once() {
        let mut tracker = Tracker::default();
        let poll = Duration::from_secs(1);
        let call = busy(1, Kind::KeyBinding);
        let mut notes = Vec::new();
        for _ in 0..40 {
            notes.extend(tracker.look(call, poll, poll, budgets, || Some(sample(b'D'))));
        }
        assert_eq!(notes, [Note::Held("in the kernel (disk or swap)")]);
        assert_eq!(tracker.counted, Duration::ZERO);
    }

    #[test]
    fn proc_files_parse() {
        let stat = "1154 (a) (b) c) S 1 1154 1154 0 -1 4194560 81 0 7 0 1234 56 0 0 26 -12 9";
        assert_eq!(parse_stat(stat), Some((b'S', 81, 7, 1234)));
        assert_eq!(parse_schedstat("123456 789 42\n"), Some((123456, 789)));
        let psi = "some avg10=1.00 avg60=0.50 avg300=0.10 total=5\nfull avg10=12.50 avg60=1.00 avg300=0.20 total=3\n";
        assert_eq!(parse_avg10(psi, "full"), Some(12.5));
        assert_eq!(parse_avg10(psi, "some"), Some(1.0));
        assert_eq!(parse_avg10("someone avg10=9\n", "some"), None);
        assert!(parse_traced("Name:\tx\nTracerPid:\t4242\n"));
        assert!(!parse_traced("Name:\tx\nTracerPid:\t0\n"));
    }

    #[test]
    fn the_environment_only_turns_it_down() {
        for off in ["off", "0", "false", " off "] {
            assert!(env_says_off(Some(off)), "{off}");
        }
        for on in [None, Some("on"), Some("1"), Some("abort"), Some("")] {
            assert!(!env_says_off(on), "{on:?}");
        }
    }

    #[test]
    fn each_session_is_told_of_the_hangs_since_the_last_one() {
        let dir = std::env::temp_dir().join(format!("shojiwm-wdog-unseen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join(MARKER_FILE);
        let append = |line: &str| append_line(&marker, line).unwrap();
        let key = |n: u32, pid: u32| format!("@{n} 2026-10-06T00:00:{n:02}Z pid {pid}");
        let abort = |n: u32, pid: u32| format!("{} abort: config call `schedulerTick` hung", key(n, pid));
        let stood_down = |n: u32, from: u32, pid: u32| {
            format!("@{n} 2026-10-06T00:00:{n:02}Z stood down from {}: the call returned", key(from, pid))
        };

        // No marker yet: nothing to tell, and no sidecar written.
        assert!(take_unseen(&dir).is_empty());
        assert!(!dir.join(SEEN_FILE).exists());

        // An abort the watchdog stood down from ended nothing, whichever of
        // the two lines reached the disk first.
        append(&abort(1, 1));
        append(&stood_down(2, 1, 1));
        append(&stood_down(4, 3, 1));
        append(&abort(3, 1));
        // The same pid again (a later boot), with no stand-down: news.
        append(&abort(5, 1));
        assert_eq!(take_unseen(&dir), [abort(5, 1)]);
        assert!(take_unseen(&dir).is_empty(), "the next session has seen it");

        for n in 6..13 {
            append(&abort(n, 2));
        }
        let last_five: Vec<String> = (8..13).map(|n| abort(n, 2)).collect();
        assert_eq!(take_unseen(&dir), last_five, "the last five at most");

        // A line still being written is read next time, whole.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&marker)
            .unwrap()
            .write_all(abort(13, 2).as_bytes())
            .unwrap();
        assert!(take_unseen(&dir).is_empty());
        std::fs::OpenOptions::new().append(true).open(&marker).unwrap().write_all(b"\n").unwrap();
        assert_eq!(take_unseen(&dir), [abort(13, 2)]);

        // A marker deleted by hand and grown again past the old offset is
        // read from the start, not from that offset.
        let fresh: Vec<String> = (20..40).map(|n| abort(n, 9)).collect();
        std::fs::write(&marker, fresh.join("\n") + "\n").unwrap();
        assert_eq!(take_unseen(&dir), fresh[15..].to_vec());

        // So is one cut short.
        std::fs::write(&marker, format!("{}\n", abort(1, 3))).unwrap();
        assert_eq!(take_unseen(&dir), [abort(1, 3)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn utc_stamps() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc(1_791_490_443), "2026-10-08T20:14:03Z");
    }

    #[test]
    fn requests_are_named_like_the_typescript_runtime() {
        let named = |request| {
            let (kind, label) = describe(request);
            (kind_name(kind as u8), label.map(str::to_owned))
        };
        assert_eq!(
            named(R::Input(I::KeyBinding { binding_id: "b" })),
            ("invokeKeyBinding", Some("b".into()))
        );
        assert_eq!(
            named(R::Decoration(D::InvokeHandler {
                window_id: "w",
                handler_id: "h"
            })),
            ("invokeHandler", Some("h".into()))
        );
        assert_eq!(
            named(R::Decoration(D::StartClose { window_id: "w" })),
            ("startClose", Some("w".into()))
        );
        assert_eq!(
            named(R::Decoration(D::Closed { window_id: "w" })),
            ("windowClosed", Some("w".into()))
        );
        assert_eq!(
            named(R::Decoration(D::EvaluateCached {
                window_id: "w",
                window: None,
                force_full: false
            })),
            ("evaluateCached", None)
        );
        assert_eq!(named(R::SchedulerTick), ("schedulerTick", None));
        assert_eq!(named(R::Effect(E::Background)), ("getEffectConfig", None));
        assert_eq!(
            named(R::Effect(E::Layers {
                output_name: "o",
                layers: &[]
            })),
            ("evaluateLayerEffects", None)
        );
        assert_eq!(
            named(R::Effect(E::Popups {
                output_name: "o",
                popups: &[]
            })),
            ("evaluatePopupEffects", None)
        );
    }

    #[test]
    fn the_report_says_loop_or_wait() {
        let report = |running, blocked| HangReport {
            kind: "invokeKeyBinding",
            label: Some("spin".into()),
            counted: Duration::from_secs(10),
            in_flight: Duration::from_secs(11),
            tid: 1154,
            running,
            blocked,
            held: 1,
        };
        let text = report(10, 0).to_string();
        assert!(
            text.starts_with(
                "config call `invokeKeyBinding` [spin] has not returned: 10.0s counted"
            ),
            "{text}"
        );
        assert!(
            text.contains("thread 1154 was running: probably an endless loop"),
            "{text}"
        );
        assert!(report(0, 10).to_string().contains("deadlock"));
    }

    // -- in process, with the action replaced by a callback -------------------

    /// How long a probe stays stuck at most: a watchdog that never reports
    /// fails its test (the report is checked once the call returns) rather
    /// than hanging it.
    const STUCK_AT_MOST: Duration = Duration::from_secs(20);

    fn stuck(release: &AtomicBool, spin: bool) {
        let deadline = Instant::now() + STUCK_AT_MOST;
        while !release.load(Acquire) && Instant::now() < deadline {
            if spin {
                std::hint::spin_loop();
            } else {
                thread::sleep(MS);
            }
        }
    }

    /// Key binding "spin" spins and scheduler ticks sleep until released;
    /// "nap" takes 1 ms; enable takes `enable_for`.
    struct Probe {
        release: Arc<AtomicBool>,
        enable_for: Duration,
    }

    impl ConfigRuntime for Probe {
        fn enable(&mut self) -> Result<(), RuntimeError> {
            thread::sleep(self.enable_for);
            Ok(())
        }

        fn request(&mut self, _: f64, request: R<'_>) -> Result<RuntimeReply, RuntimeError> {
            match request {
                R::Input(I::KeyBinding { binding_id: "spin" }) => stuck(&self.release, true),
                R::Input(I::KeyBinding { binding_id: "nap" }) => thread::sleep(MS),
                // Like an IPC handler drained inside the call, which hangs.
                R::Input(I::KeyBinding { binding_id: "noted" }) => {
                    let _noted = note("ipc test.method");
                    stuck(&self.release, false);
                }
                // A handler that returned, then the call itself hangs.
                R::Input(I::KeyBinding { binding_id: "noted-then-stuck" }) => {
                    drop(note("ipc test.method"));
                    stuck(&self.release, false);
                }
                R::SchedulerTick => stuck(&self.release, false),
                _ => {}
            }
            Ok(RuntimeReply::Done)
        }
    }

    /// Small budgets; the hook reports and releases the stuck call.
    fn quick(release: &Arc<AtomicBool>) -> (HangWatchdog, mpsc::Receiver<HangReport>) {
        let (sender, reports) = mpsc::channel();
        let release = Arc::clone(release);
        let hook = move |report: &HangReport| {
            let _ = sender.send(report.clone());
            release.store(true, Release);
        };
        let watchdog = HangWatchdog::default()
            .call_budget(100 * MS)
            .lifecycle_budget(400 * MS)
            .poll(10 * MS)
            .on_hang(OnHang::Call(Arc::new(hook)));
        (watchdog, reports)
    }

    fn probe(watchdog: HangWatchdog, release: &Arc<AtomicBool>) -> Box<dyn ConfigRuntime> {
        let release = Arc::clone(release);
        launch(Some(watchdog), move || Probe {
            release,
            enable_for: 150 * MS,
        })
    }

    #[test]
    fn a_spinning_key_binding_is_reported_once_with_its_id() {
        let release = Arc::new(AtomicBool::new(false));
        // Stays stuck ten looks past the report, so a second one would show.
        let (sender, reports) = mpsc::channel();
        let later = Arc::clone(&release);
        let hook = move |report: &HangReport| {
            let _ = sender.send(report.clone());
            let release = Arc::clone(&later);
            thread::spawn(move || {
                thread::sleep(100 * MS);
                release.store(true, Release);
            });
        };
        let (watchdog, _) = quick(&release);
        let mut runtime = probe(watchdog.on_hang(OnHang::Call(Arc::new(hook))), &release);
        runtime.enable().unwrap(); // 150 ms: inside the lifecycle budget
        assert!(reports.try_recv().is_err(), "enable got the call budget");
        // SAFETY: gettid has no preconditions.
        let tid = unsafe { libc::gettid() };
        runtime
            .request(0.0, R::Input(I::KeyBinding { binding_id: "spin" }))
            .unwrap();
        let fired: Vec<HangReport> = reports.try_iter().collect();
        assert_eq!(fired.len(), 1, "reported once: {fired:?}");
        let report = &fired[0];
        assert_eq!(
            (report.kind, report.label.as_deref(), report.tid),
            ("invokeKeyBinding", Some("spin"), tid)
        );
        assert!(report.running > report.blocked, "{report}");
    }

    #[test]
    fn a_blocked_call_is_reported_as_asleep() {
        let release = Arc::new(AtomicBool::new(false));
        let (watchdog, reports) = quick(&release);
        let mut runtime = probe(watchdog, &release);
        runtime
            .request(0.0, R::Input(I::KeyBinding { binding_id: "nap" }))
            .unwrap();
        runtime.request(0.0, R::SchedulerTick).unwrap();
        let report = reports.try_recv().expect("the stuck call was never reported");
        assert_eq!(report.kind, "schedulerTick");
        assert_eq!(report.label, None, "blamed the earlier key binding");
        assert!(report.blocked > report.running, "{report}");
        assert!(report.to_string().contains("deadlock"));
    }

    #[test]
    fn a_hang_inside_a_handler_names_the_handler() {
        for (binding, label) in [("noted", "ipc test.method"), ("noted-then-stuck", "noted-then-stuck")] {
            let release = Arc::new(AtomicBool::new(false));
            let (watchdog, reports) = quick(&release);
            let mut runtime = probe(watchdog, &release);
            runtime
                .request(0.0, R::Input(I::KeyBinding { binding_id: binding }))
                .unwrap();
            // Between calls, with the watchdog armed, there is nothing to name.
            assert!(note("ipc idle").is_none(), "{binding}");
            let report = reports.try_recv().expect("the stuck call was never reported");
            assert_eq!(report.label.as_deref(), Some(label), "{binding}");
        }
        // With no runtime at all, neither.
        assert!(note("ipc idle").is_none());
    }

    #[test]
    fn many_short_calls_never_fire() {
        let release = Arc::new(AtomicBool::new(false));
        let (watchdog, reports) = quick(&release);
        let mut runtime = probe(watchdog, &release);
        for _ in 0..500 {
            runtime
                .request(0.0, R::Input(I::KeyBinding { binding_id: "nap" }))
                .unwrap();
        }
        assert!(reports.try_recv().is_err());
    }

    #[test]
    fn a_hang_while_launching_is_reported_as_launch() {
        let release = Arc::new(AtomicBool::new(false));
        let (watchdog, reports) = quick(&release);
        let stuck = Arc::clone(&release);
        let _runtime = launch(Some(watchdog), move || {
            self::stuck(&stuck, false);
            Probe {
                release: stuck,
                enable_for: Duration::ZERO,
            }
        });
        assert_eq!(
            reports.try_recv().expect("the stuck launch was never reported").kind,
            "launch"
        );
    }

    #[test]
    fn dropping_the_runtime_stops_the_watcher_at_once() {
        let release = Arc::new(AtomicBool::new(false));
        let (watchdog, reports) = quick(&release);
        let runtime = probe(watchdog.poll(Duration::from_secs(30)), &release);
        let started = Instant::now();
        drop(runtime);
        assert!(started.elapsed() < Duration::from_secs(1));
        // The hook, and so its sender, lived on the watcher thread.
        assert!(
            matches!(reports.try_recv(), Err(mpsc::TryRecvError::Disconnected)),
            "the watcher outlived its runtime"
        );
    }

    // -- the real abort path, in a child process ------------------------------

    const CHILD: &str = "SHOJIWM_WATCHDOG_CHILD";
    const CHILD_DIR: &str = "SHOJIWM_WATCHDOG_CHILD_DIR";

    /// Re-run test `name` alone in a child with `scenario`; returns its exit
    /// status, stderr and marker text, and how long it ran.
    fn run_child(
        name: &str,
        scenario: &str,
    ) -> (std::process::ExitStatus, String, String, Duration) {
        let dir = std::env::temp_dir().join(format!(
            "shojiwm-watchdog-{}-{scenario}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let test = format!(
            "{}::{name}",
            module_path!()
                .split_once("::")
                .map_or(module_path!(), |(_, path)| path)
        );
        let started = Instant::now();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([test.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD, scenario)
            .env(CHILD_DIR, &dir)
            .env_remove("SHOJI_RUNTIME_WATCHDOG")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() > Duration::from_secs(30) {
                let _ = child.kill(); // this child, by pid
                panic!("the child never ended");
            }
            thread::sleep(10 * MS);
        };
        let elapsed = started.elapsed();
        let mut stderr = String::new();
        std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
        let marker = std::fs::read_to_string(dir.join(MARKER_FILE)).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        (status, stderr, marker, elapsed)
    }

    /// In the child: no core for systemd-coredump, then hang for real.
    fn child_hangs(scenario: &str) {
        // SAFETY: prctl(PR_SET_DUMPABLE) only changes this process's flag.
        unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
        let dir = PathBuf::from(std::env::var_os(CHILD_DIR).unwrap());
        let marker = dir.join(MARKER_FILE);
        let watchdog = HangWatchdog::default()
            .call_budget(300 * MS)
            .poll(20 * MS)
            .report_dir(&dir);
        let sigabrt = |how: libc::c_int| {
            // SAFETY: a zeroed sigset_t is valid input to sigemptyset.
            unsafe {
                let mut set: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, libc::SIGABRT);
                libc::pthread_sigmask(how, &set, std::ptr::null_mut());
            }
        };
        // Blocked here before launch, so the watcher (and its report helper)
        // inherit the block: only a signal aimed at this thread is taken at
        // once, and one aimed anywhere else waits for the fallback. The
        // "blocked-signal" scenario keeps it blocked here too.
        sigabrt(libc::SIG_BLOCK);
        struct Child(PathBuf, bool);
        impl ConfigRuntime for Child {
            fn request(&mut self, _: f64, _: R<'_>) -> Result<RuntimeReply, RuntimeError> {
                if !self.1 {
                    loop {
                        std::hint::spin_loop();
                    }
                }
                // Stand-down: return once the report is on disk, before the signal.
                while !std::fs::read_to_string(&self.0).is_ok_and(|text| text.contains(" abort: "))
                {
                    thread::sleep(5 * MS);
                }
                Ok(RuntimeReply::Done)
            }
        }
        let stand_down = scenario == "stand-down";
        let mut runtime = launch(Some(watchdog), || Child(marker.clone(), stand_down));
        if scenario != "blocked-signal" {
            sigabrt(libc::SIG_UNBLOCK);
        }
        // SAFETY: gettid has no preconditions.
        eprintln!("child thread {}", unsafe { libc::gettid() });
        runtime
            .request(0.0, R::Input(I::KeyBinding { binding_id: "spin" }))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::fs::read_to_string(&marker)
            .unwrap_or_default()
            .contains("stood down")
        {
            assert!(Instant::now() < deadline, "never stood down");
            thread::sleep(10 * MS);
        }
    }

    #[test]
    fn abort_ends_the_process_with_sigabrt_on_the_stuck_thread() {
        if let Ok(scenario) = std::env::var(CHILD) {
            return child_hangs(&scenario);
        }
        let (status, stderr, marker, _) = run_child(
            "abort_ends_the_process_with_sigabrt_on_the_stuck_thread",
            "spin",
        );
        assert_eq!(status.signal(), Some(libc::SIGABRT), "{stderr}");
        assert!(
            !marker.contains(" fallback from "),
            "the stuck thread never took the signal: {marker}"
        );
        assert!(
            marker.contains(" abort: config call `invokeKeyBinding` [spin]"),
            "{marker}"
        );
        let tid = stderr
            .lines()
            .find_map(|line| line.strip_prefix("child thread "))
            .unwrap();
        assert!(marker.contains(&format!("thread {tid} was")), "{marker}");
        assert!(stderr.contains("shoji_wm: @"), "{stderr}");
    }

    #[test]
    fn abort_falls_back_when_the_thread_blocks_sigabrt() {
        if let Ok(scenario) = std::env::var(CHILD) {
            return child_hangs(&scenario);
        }
        let (status, stderr, marker, elapsed) = run_child(
            "abort_falls_back_when_the_thread_blocks_sigabrt",
            "blocked-signal",
        );
        assert_eq!(status.signal(), Some(libc::SIGABRT), "{stderr}");
        assert!(elapsed >= ABORT_FALLBACK, "{elapsed:?}");
        assert!(marker.contains(" fallback from "), "{marker}");
    }

    #[test]
    fn abort_stands_down_when_the_call_returns_during_the_report() {
        if let Ok(scenario) = std::env::var(CHILD) {
            return child_hangs(&scenario);
        }
        let (status, stderr, marker, _) = run_child(
            "abort_stands_down_when_the_call_returns_during_the_report",
            "stand-down",
        );
        assert!(status.success(), "{status:?} {stderr}");
        assert!(
            marker.contains(" abort: ") && marker.contains(" stood down from "),
            "{marker}"
        );
    }
}
