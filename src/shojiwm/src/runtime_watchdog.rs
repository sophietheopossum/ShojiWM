//! Watchdog for the embedded config runtime.
//!
//! The compositor thread waits synchronously for most config-runtime answers.
//! A config handler that never returns (an endless loop, or an await that never
//! resolves) used to block that thread forever: no input, no rendering, and no
//! Super+Shift+R, because that key is handled on the same thread. The watchdog
//! bounds every wait, stops a runaway isolate, and lets the caller report the
//! stop so the session keeps running on basic decorations until a reload.

use std::{
    sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

use tracing::{info, warn};

/// How long a single wait slice lasts. The budget is spent in slices so that
/// a stopped process (SIGSTOP, a frozen cgroup, a debugger) resuming after a
/// long pause is charged at most `SLICE_CREDIT_CAP`, not the whole pause.
const WAIT_SLICE: Duration = Duration::from_millis(250);
const SLICE_CREDIT_CAP: Duration = Duration::from_millis(500);
/// How long a stopped runtime thread gets to exit before it is detached.
pub(crate) const TEARDOWN_AFTER_KILL: Duration = Duration::from_millis(500);
const SLOW_TRIP_LOG_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
pub struct RuntimeWatchdog {
    /// `false` keeps waiting (and logs) instead of stopping the runtime.
    pub enabled: bool,
    /// Budget for a request once the config has loaded. The slowest handler
    /// trip on record is 835 ms, in a session deep in swap.
    pub hang: Duration,
    /// Budget while the session's first config generation loads (isolate
    /// start, preload, config import). Cold boot competes with font and GPU
    /// setup, so it is generous: there is nothing to fall back to yet.
    pub boot_load_hang: Duration,
    /// Budget while a reloaded config loads. A whole reload takes about 2 s,
    /// so a config that still hangs costs this much per Super+Shift+R.
    pub load_hang: Duration,
    /// Log once when a request has waited this long (loaded / loading).
    pub stuck_warn: Duration,
    pub load_stuck_warn: Duration,
    /// How long a closed runtime gets to exit before it is stopped.
    pub teardown_grace: Duration,
    /// Round trips slower than this are reported, per request class.
    pub slow_frame: Duration,
    pub slow_discrete: Duration,
}

impl Default for RuntimeWatchdog {
    fn default() -> Self {
        Self {
            enabled: true,
            hang: Duration::from_secs(5),
            boot_load_hang: Duration::from_secs(30),
            load_hang: Duration::from_secs(15),
            stuck_warn: Duration::from_secs(1),
            load_stuck_warn: Duration::from_secs(10),
            teardown_grace: Duration::from_secs(5),
            slow_frame: Duration::from_millis(4),
            slow_discrete: Duration::from_millis(16),
        }
    }
}

impl RuntimeWatchdog {
    /// `SHOJI_RUNTIME_WATCHDOG=off` keeps every wait unbounded, as before the
    /// watchdog existed, but still logs where it would have stopped.
    pub fn from_env() -> Self {
        Self::from_env_value(std::env::var("SHOJI_RUNTIME_WATCHDOG").ok().as_deref())
    }

    fn from_env_value(value: Option<&str>) -> Self {
        let enabled = !matches!(value.map(str::trim), Some("off" | "0" | "false"));
        Self {
            enabled,
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestClass {
    /// Sent while a frame or an input event waits on it.
    Frame,
    /// Sent once per user action or window event.
    Discrete,
    /// Loads or unloads the config; legitimately slow, never warned about.
    Lifecycle,
}

/// Request kinds as the runtime names them. The index doubles as a bit in the
/// per-isolate "seen" set, so this stays under 64 entries.
const REQUEST_KINDS: &[(&str, RequestClass)] = &[
    ("request", RequestClass::Discrete),
    ("evaluate", RequestClass::Discrete),
    ("evaluatePreview", RequestClass::Discrete),
    ("evaluateCached", RequestClass::Frame),
    ("schedulerTick", RequestClass::Frame),
    ("getEffectConfig", RequestClass::Discrete),
    ("evaluateLayerEffects", RequestClass::Frame),
    ("evaluatePopupEffects", RequestClass::Frame),
    ("pointerMove", RequestClass::Frame),
    ("pointerMoveAsync", RequestClass::Frame),
    ("gestureSwipe", RequestClass::Frame),
    ("gestureSwipeAsync", RequestClass::Frame),
    ("windowMove", RequestClass::Frame),
    ("windowResize", RequestClass::Frame),
    ("drainPreload", RequestClass::Lifecycle),
    ("lifecycleEnable", RequestClass::Lifecycle),
    ("lifecycleDisable", RequestClass::Lifecycle),
    ("windowDecorationPolicy", RequestClass::Discrete),
    ("windowClosed", RequestClass::Discrete),
    ("startClose", RequestClass::Discrete),
    ("invokeHandler", RequestClass::Discrete),
    ("invokeKeyBinding", RequestClass::Discrete),
    ("windowMaximizeRequest", RequestClass::Discrete),
    ("windowMinimizeRequest", RequestClass::Discrete),
    ("windowFullscreenRequest", RequestClass::Discrete),
    ("windowActivateRequest", RequestClass::Discrete),
    ("workspaceActivate", RequestClass::Discrete),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RequestKind(u8);

impl RequestKind {
    pub(crate) const UNKNOWN: Self = Self(0);

    /// Unknown names map to `UNKNOWN`, which is classed as discrete.
    pub(crate) fn named(name: &str) -> Self {
        REQUEST_KINDS
            .iter()
            .position(|(kind, _)| *kind == name)
            .map_or(Self::UNKNOWN, |index| Self(index as u8))
    }

    pub(crate) fn name(self) -> &'static str {
        REQUEST_KINDS[usize::from(self.0)].0
    }

    pub(crate) fn class(self) -> RequestClass {
        REQUEST_KINDS[usize::from(self.0)].1
    }

    pub(crate) fn bit(self) -> u64 {
        1 << self.0
    }
}

/// The request currently waiting for an answer.
#[derive(Debug)]
pub(crate) struct InFlight {
    pub kind: RequestKind,
    /// The binding or handler id, when the request names one.
    pub label: Option<String>,
    pub written_at: Instant,
    pub written_at_ns: u64,
    /// Position of this request in the isolate's request stream.
    pub seq: u64,
}

/// What stops a runaway isolate: V8 termination for running JS, and the
/// runtime's cancellation token for one parked on an await that never
/// resolves (termination alone only takes effect when JS next runs).
pub(crate) struct StopLever {
    terminate: Box<dyn Fn() + Send + Sync>,
    cancel: Box<dyn Fn() + Send + Sync>,
}

impl StopLever {
    pub(crate) fn new(
        terminate: impl Fn() + Send + Sync + 'static,
        cancel: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            terminate: Box::new(terminate),
            cancel: Box::new(cancel),
        }
    }

    fn pull(&self) {
        (self.terminate)();
        (self.cancel)();
    }
}

#[derive(Default)]
struct ControlState {
    kill_reason: Option<String>,
    lever: Option<StopLever>,
    exited: bool,
    abandoned: bool,
}

/// Shared between the owner of one isolate and the thread running it.
///
/// Arming and killing take the same lock, so a kill that lands before the
/// isolate exists fires the moment the lever is armed.
#[derive(Default)]
pub(crate) struct RuntimeControl {
    state: Mutex<ControlState>,
    exited_changed: Condvar,
    killed: AtomicBool,
    stop_reported: AtomicBool,
    /// Requests the runtime has taken off its channel, and when it took the
    /// latest one: tells "the handler is stuck" from "it never got there".
    dequeued: AtomicU64,
    dequeued_at_ns: AtomicU64,
    /// Kernel thread id of the runtime thread, for its CPU time.
    tid: AtomicI32,
}

impl RuntimeControl {
    fn state(&self) -> std::sync::MutexGuard<'_, ControlState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn arm(&self, lever: StopLever) {
        let mut state = self.state();
        if state.kill_reason.is_some() {
            lever.pull();
        }
        state.lever = Some(lever);
    }

    /// Returns false if the runtime was already killed.
    pub(crate) fn kill(&self, reason: String) -> bool {
        let mut state = self.state();
        if state.kill_reason.is_some() {
            return false;
        }
        state.kill_reason = Some(reason);
        self.killed.store(true, Ordering::Release);
        if let Some(lever) = state.lever.as_ref() {
            lever.pull();
        }
        true
    }

    pub(crate) fn is_killed(&self) -> bool {
        self.killed.load(Ordering::Acquire)
    }

    pub(crate) fn kill_reason(&self) -> Option<String> {
        if !self.is_killed() {
            return None;
        }
        self.state().kill_reason.clone()
    }

    /// True for exactly one caller after a kill, so the stop is reported once.
    pub(crate) fn claim_stop_report(&self) -> bool {
        self.is_killed() && !self.stop_reported.swap(true, Ordering::AcqRel)
    }

    pub(crate) fn mark_exited(&self) {
        let mut state = self.state();
        state.exited = true;
        if state.abandoned {
            info!("a detached config runtime thread has finally exited");
        }
        self.exited_changed.notify_all();
    }

    pub(crate) fn wait_exited(&self, timeout: Duration) -> bool {
        let state = self.state();
        let (state, _) = self
            .exited_changed
            .wait_timeout_while(state, timeout, |state| !state.exited)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.exited
    }

    /// Record that the owner gave up on a thread that has not exited.
    pub(crate) fn abandon(&self) {
        let mut state = self.state();
        if !state.exited {
            state.abandoned = true;
        }
    }

    pub(crate) fn note_dequeued(&self) {
        self.dequeued_at_ns.store(monotonic_ns(), Ordering::Release);
        self.dequeued.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn dequeued(&self) -> u64 {
        self.dequeued.load(Ordering::Acquire)
    }

    pub(crate) fn dequeued_at_ns(&self) -> u64 {
        self.dequeued_at_ns.load(Ordering::Acquire)
    }

    pub(crate) fn set_current_thread(&self) {
        // SAFETY: gettid has no preconditions.
        let tid = unsafe { libc::gettid() };
        self.tid.store(tid, Ordering::Release);
    }

    pub(crate) fn tid(&self) -> Option<i32> {
        let tid = self.tid.load(Ordering::Acquire);
        (tid > 0).then_some(tid)
    }
}

/// Marks the runtime thread exited when it drops. It must be the first value
/// bound in the thread, so it drops after the isolate is gone.
pub(crate) struct ExitGuard(pub std::sync::Arc<RuntimeControl>);

impl Drop for ExitGuard {
    fn drop(&mut self) {
        self.0.mark_exited();
    }
}

pub(crate) fn monotonic_ns() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    let base = *BASE.get_or_init(Instant::now);
    u64::try_from(Instant::now().duration_since(base).as_nanos()).unwrap_or(u64::MAX)
}

/// CPU time the thread has used, from the first field of its schedstat.
pub(crate) fn thread_cpu_ns(tid: Option<i32>) -> Option<u64> {
    let tid = tid?;
    let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/schedstat")).ok()?;
    stat.split_whitespace().next()?.parse().ok()
}

pub(crate) enum WaitOutcome<T> {
    Received(T),
    Disconnected,
    TimedOut,
}

/// Wait on `rx` for at most `budget` of credited time, calling `on_slice`
/// with the credited total after every slice that passes without an answer.
pub(crate) fn wait_credited<T>(
    rx: &Receiver<T>,
    budget: Duration,
    mut on_slice: impl FnMut(Duration),
) -> WaitOutcome<T> {
    let mut credited = Duration::ZERO;
    loop {
        let slice_started = Instant::now();
        match rx.recv_timeout(WAIT_SLICE) {
            Ok(value) => return WaitOutcome::Received(value),
            Err(RecvTimeoutError::Disconnected) => return WaitOutcome::Disconnected,
            Err(RecvTimeoutError::Timeout) => {
                credited += slice_started.elapsed().min(SLICE_CREDIT_CAP);
                on_slice(credited);
                if credited >= budget {
                    // An answer that landed exactly at the deadline still counts.
                    return match rx.try_recv() {
                        Ok(value) => WaitOutcome::Received(value),
                        Err(_) => WaitOutcome::TimedOut,
                    };
                }
            }
        }
    }
}

/// The text shown in the config error overlay when the watchdog stops the
/// runtime.
pub(crate) fn hang_report(
    in_flight: Option<&InFlight>,
    budget: Duration,
    picked_up: bool,
    cpu_ratio: Option<f64>,
    last_done: Option<(RequestKind, Instant)>,
) -> String {
    let kind = in_flight.map_or(RequestKind::UNKNOWN, |request| request.kind).name();
    let label = in_flight
        .and_then(|request| request.label.as_deref())
        .map(|label| format!(" ({label})"))
        .unwrap_or_default();
    let cause = if !picked_up {
        match last_done {
            Some((last, at)) => format!(
                "The config runtime never got to `{kind}`: other config code (an IPC handler, \
                 a timer or a promise callback) has kept it busy since it answered `{}` {:.1?} \
                 ago.",
                last.name(),
                at.elapsed()
            ),
            None => format!(
                "The config runtime never got to `{kind}`: other config code has kept it busy \
                 since it started."
            ),
        }
    } else {
        match cpu_ratio {
            Some(ratio) if ratio >= 0.8 => format!(
                "The config's `{kind}` handler{label} ran for {budget:?} without returning, \
                 probably an endless loop."
            ),
            Some(ratio) if ratio <= 0.1 => format!(
                "The config's `{kind}` handler{label} waited {budget:?} for something that never \
                 happened, such as an await that never resolves."
            ),
            _ => format!("The config's `{kind}` handler{label} did not return within {budget:?}."),
        }
    };
    format!(
        "{cause} ShojiWM stopped the config runtime. Windows and input keep working with basic \
         decorations; config key bindings, tiling and animations are paused until you fix the \
         config and press Super+Shift+R."
    )
}

/// Pull `"bindingId"` or `"handlerId"` out of a JSON request for the report.
pub(crate) fn request_label(request: &str) -> Option<String> {
    let mut end = request.len().min(512);
    while !request.is_char_boundary(end) {
        end -= 1;
    }
    let head = &request[..end];
    ["\"bindingId\":\"", "\"handlerId\":\""]
        .iter()
        .find_map(|key| {
            let start = head.find(key)? + key.len();
            let rest = &head[start..];
            Some(rest[..rest.find('"')?].to_owned())
        })
}

struct SlowTrip {
    kind: RequestKind,
    count: u32,
    max_total: Duration,
    max_queued: Duration,
    label: Option<String>,
    after: Option<RequestKind>,
}

struct SlowTripLog {
    last_emit: Option<Instant>,
    trips: Vec<SlowTrip>,
}

static SLOW_TRIPS: Mutex<SlowTripLog> = Mutex::new(SlowTripLog {
    last_emit: None,
    trips: Vec::new(),
});
/// Slow trips are collected but not yet logged.
static SLOW_TRIPS_PENDING: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(crate) static SLOW_TRIP_COUNT: AtomicU64 = AtomicU64::new(0);

/// Collect a slow round trip; emit at most one summary per interval, and the
/// first slow trip after a quiet interval at once.
///
/// `queued` is the part spent before the runtime picked the request up. When
/// it dominates, the runtime was busy with whatever followed `after`.
pub(crate) fn record_slow_trip(
    kind: RequestKind,
    label: Option<&str>,
    total: Duration,
    queued: Duration,
    after: Option<RequestKind>,
) {
    #[cfg(test)]
    SLOW_TRIP_COUNT.fetch_add(1, Ordering::Relaxed);

    let mut log = SLOW_TRIPS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let existing = log.trips.iter().position(|trip| trip.kind == kind);
    match existing {
        Some(index) => {
            let trip = &mut log.trips[index];
            trip.count += 1;
            if total > trip.max_total {
                trip.max_total = total;
                trip.max_queued = queued;
                trip.label = label.map(str::to_owned);
                trip.after = after;
            }
        }
        None if log.trips.len() < 8 => log.trips.push(SlowTrip {
            kind,
            count: 1,
            max_total: total,
            max_queued: queued,
            label: label.map(str::to_owned),
            after,
        }),
        None => {}
    }
    if log
        .last_emit
        .is_some_and(|at| at.elapsed() < SLOW_TRIP_LOG_INTERVAL)
    {
        SLOW_TRIPS_PENDING.store(true, Ordering::Release);
        return;
    }
    emit_slow_trips(&mut log);
}

/// Log slow trips collected inside the last interval once it has passed, so
/// the worst trip of a burst is not held back until some later slow trip.
/// Called after every answered request; costs one atomic load when idle.
pub(crate) fn flush_slow_trips_if_due() {
    if !SLOW_TRIPS_PENDING.load(Ordering::Acquire) {
        return;
    }
    let mut log = SLOW_TRIPS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if log.trips.is_empty()
        || log
            .last_emit
            .is_some_and(|at| at.elapsed() < SLOW_TRIP_LOG_INTERVAL)
    {
        return;
    }
    emit_slow_trips(&mut log);
}

fn emit_slow_trips(log: &mut SlowTripLog) {
    let summary = log
        .trips
        .iter()
        .map(|trip| {
            let label = trip
                .label
                .as_deref()
                .map(|label| format!(" [{label}]"))
                .unwrap_or_default();
            let queued = if trip.max_queued * 2 > trip.max_total {
                match trip.after {
                    Some(after) => format!(
                        " (queued {:.1?} behind work after `{}`)",
                        trip.max_queued,
                        after.name()
                    ),
                    None => format!(" (queued {:.1?})", trip.max_queued),
                }
            } else {
                String::new()
            };
            format!(
                "{} n={} max={:.1?}{queued}{label}",
                trip.kind.name(),
                trip.count,
                trip.max_total
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    warn!(
        target: "shoji_wm::runtime_watchdog",
        trips = %summary,
        "slow config runtime round trips since the last report"
    );
    log.trips.clear();
    log.last_emit = Some(Instant::now());
    SLOW_TRIPS_PENDING.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicUsize, mpsc};

    fn counting_lever(count: &Arc<AtomicUsize>) -> StopLever {
        let terminate = Arc::clone(count);
        let cancel = Arc::clone(count);
        StopLever::new(
            move || {
                terminate.fetch_add(1, Ordering::SeqCst);
            },
            move || {
                cancel.fetch_add(1, Ordering::SeqCst);
            },
        )
    }

    #[test]
    fn runtime_watchdog_unit_kill_before_arm_fires_on_arm() {
        let control = RuntimeControl::default();
        let pulls = Arc::new(AtomicUsize::new(0));
        assert!(control.kill("stop".into()));
        assert_eq!(pulls.load(Ordering::SeqCst), 0);
        control.arm(counting_lever(&pulls));
        assert_eq!(pulls.load(Ordering::SeqCst), 2);
        assert_eq!(control.kill_reason().as_deref(), Some("stop"));
    }

    #[test]
    fn runtime_watchdog_unit_second_kill_is_a_no_op() {
        let control = RuntimeControl::default();
        let pulls = Arc::new(AtomicUsize::new(0));
        control.arm(counting_lever(&pulls));
        assert!(control.kill("first".into()));
        assert!(!control.kill("second".into()));
        assert_eq!(pulls.load(Ordering::SeqCst), 2);
        assert_eq!(control.kill_reason().as_deref(), Some("first"));
        assert!(control.claim_stop_report());
        assert!(!control.claim_stop_report());
    }

    #[test]
    fn runtime_watchdog_unit_exit_wakes_waiter() {
        let control = Arc::new(RuntimeControl::default());
        assert!(!control.wait_exited(Duration::from_millis(10)));
        let guard = ExitGuard(Arc::clone(&control));
        let waiter = {
            let control = Arc::clone(&control);
            std::thread::spawn(move || control.wait_exited(Duration::from_secs(5)))
        };
        drop(guard);
        assert!(waiter.join().unwrap());
    }

    #[test]
    fn runtime_watchdog_unit_env_switch() {
        assert!(RuntimeWatchdog::from_env_value(None).enabled);
        assert!(RuntimeWatchdog::from_env_value(Some("on")).enabled);
        assert!(RuntimeWatchdog::from_env_value(Some("garbage")).enabled);
        assert!(!RuntimeWatchdog::from_env_value(Some("off")).enabled);
        assert!(!RuntimeWatchdog::from_env_value(Some(" 0 ")).enabled);
    }

    #[test]
    fn runtime_watchdog_unit_kinds_round_trip() {
        assert!(REQUEST_KINDS.len() < 64);
        for (name, class) in REQUEST_KINDS {
            let kind = RequestKind::named(name);
            assert_eq!(kind.name(), *name);
            assert_eq!(kind.class(), *class);
        }
        assert_eq!(RequestKind::named("noSuchKind"), RequestKind::UNKNOWN);
        assert_eq!(
            RequestKind::named("lifecycleEnable").class(),
            RequestClass::Lifecycle
        );
    }

    #[test]
    fn runtime_watchdog_unit_wait_credited_times_out_and_receives() {
        let (tx, rx) = mpsc::channel::<u8>();
        let started = Instant::now();
        let mut slices = Vec::new();
        let outcome = wait_credited(&rx, Duration::from_millis(600), |credited| {
            slices.push(credited)
        });
        assert!(matches!(outcome, WaitOutcome::TimedOut));
        assert!(slices.len() >= 2, "{slices:?}");
        assert!(slices.windows(2).all(|pair| pair[0] < pair[1]), "{slices:?}");
        assert!(slices.last().is_some_and(|last| *last >= Duration::from_millis(600)));
        assert!(started.elapsed() >= Duration::from_millis(500));

        tx.send(7).unwrap();
        assert!(matches!(
            wait_credited(&rx, Duration::from_secs(1), |_| {}),
            WaitOutcome::Received(7)
        ));
        drop(tx);
        assert!(matches!(
            wait_credited(&rx, Duration::from_secs(1), |_| {}),
            WaitOutcome::Disconnected
        ));
    }

    #[test]
    fn runtime_watchdog_unit_hang_report_shapes() {
        let request = InFlight {
            kind: RequestKind::named("invokeKeyBinding"),
            label: Some("spin".into()),
            written_at: Instant::now(),
            written_at_ns: 0,
            seq: 1,
        };
        let budget = Duration::from_secs(5);
        let looping = hang_report(Some(&request), budget, true, Some(0.99), None);
        assert!(looping.contains("`invokeKeyBinding` handler (spin)"), "{looping}");
        assert!(looping.contains("endless loop"), "{looping}");
        let waiting = hang_report(Some(&request), budget, true, Some(0.0), None);
        assert!(waiting.contains("never happened"), "{waiting}");
        let unknown = hang_report(Some(&request), budget, true, None, None);
        assert!(unknown.contains("did not return"), "{unknown}");
        let busy = hang_report(
            Some(&request),
            budget,
            false,
            None,
            Some((RequestKind::named("windowMove"), Instant::now())),
        );
        assert!(busy.contains("never got to `invokeKeyBinding`"), "{busy}");
        assert!(busy.contains("answered `windowMove`"), "{busy}");
        assert!(busy.contains("Super+Shift+R"), "{busy}");
    }

    #[test]
    fn runtime_watchdog_unit_request_label() {
        assert_eq!(
            request_label(r#"{"kind":"invokeKeyBinding","requestId":3,"bindingId":"spin"}"#)
                .as_deref(),
            Some("spin")
        );
        assert_eq!(
            request_label(r#"{"kind":"invokeHandler","handlerId":"close-7"}"#).as_deref(),
            Some("close-7")
        );
        assert_eq!(request_label(r#"{"kind":"windowClosed"}"#), None);
    }
}
