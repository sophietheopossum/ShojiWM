//! Keeps the compositor's memory in RAM, so its thread does not wait for one
//! of its own pages to come back from disk.
//!
//! The compositor draws the pointer: while it waits for a disk read, the
//! whole desktop stands still. On 26/11/2026 one session stood still for 6 to
//! 7 seconds that way. Read the next day, its compositor, which locked none
//! of its memory, had 56 MB in swap and had taken 9,848 major page faults in
//! 66,558 s of running. With
//! [`ConfigBuilder::pin_memory`](crate::ConfigBuilder::pin_memory) on, the
//! process's memory is locked at launch, and from then on a page the
//! compositor has used is not read from disk again.
//!
//! # Only with an unlimited limit
//!
//! `RLIMIT_MEMLOCK` is read first. A finite soft limit under an unlimited
//! hard limit is raised to it, `setrlimit`'s result is checked, and the limit
//! is read again. Memory is locked only if the soft limit is then unlimited.
//! With a finite hard limit nothing is changed, and one line in the log names
//! the remedy: a line `<user> hard memlock unlimited` in a file under
//! `/etc/security/limits.d/`, then a new login (systemd gives every login
//! 8 MiB).
//!
//! No finite limit is large enough to rely on. Under `MCL_FUTURE` the limit
//! is charged by mapped address space, not by RAM, and the compositor maps
//! about 2.4 GB. A request that would take the charge past the limit fails:
//! `mmap`, `mremap` and `pthread_create` with `EAGAIN`, `brk` with `ENOMEM`,
//! `malloc` by returning null, and stack growth with `SIGSEGV` (measured on
//! Linux 7.3.6 on 27/11/2026). `CAP_IPC_LOCK` is not looked at: the `CapEff`
//! bit in `/proc` is also set inside a user namespace, where `mlockall` still
//! failed with `ENOMEM`.
//!
//! # The call
//!
//! `mlockall(MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT)`, once, with no other
//! combination to fall back on. `MCL_ONFAULT` (Linux 4.4) locks a page when
//! it is first touched and reads nothing ahead. Without it the call would
//! bring in every mapping the process has, the allocator's 1 GiB arena and
//! 596 MB of file mappings among them, and afterwards every new mapping and
//! thread stack in full. `MCL_FUTURE` without `MCL_CURRENT` succeeds under a
//! finite limit and then fails allocations as described above. A failed call
//! leaves nothing locked: its errno is logged as a warning and start-up goes
//! on, with a soft limit that was raised for it left raised. After a
//! successful call the soft limit is not lowered again: with the limit below
//! what is locked, the next `mmap` fails.
//!
//! # What it does not cover
//!
//! - The first touch of a page. `MCL_ONFAULT` reads nothing ahead, so code or
//!   data used for the first time still comes from disk, once.
//! - Files read or written with ordinary system calls on the compositor
//!   thread: shader sources, decoration images and cursor shapes at their
//!   first use.
//! - GPU buffers. `mlockall` skips their mappings, and the xe driver may swap
//!   the memory behind them out by itself.
//!
//! # Not done
//!
//! - Children are not given the old soft limit back: every process the
//!   compositor starts inherits the raised soft limit. None inherits the
//!   lock, which does not pass over `fork` or `exec`.
//! - Client `wl_shm` pools are not unlocked: the pool pages the compositor
//!   has read stay in RAM while the pool is mapped. One session had 147 MiB
//!   of pools mapped on 27/11/2026.
//! - The allocator is not changed. On locked memory `madvise(MADV_DONTNEED)`
//!   fails with `EINVAL`, so heap that an allocator frees that way stays
//!   resident. mimalloc, the compositor's Rust allocator, and glibc's thread
//!   arenas free that way. In test programs on 27/11/2026, a process that
//!   freed a 4 MiB mimalloc block held 10,640 kB resident after the purge
//!   with part of the block locked and 6,476 kB with none of it locked; one
//!   that freed 30 blocks of 100 KiB from a glibc thread arena held 5,296 kB
//!   with the arena locked and 2,420 kB without. glibc's main heap is still
//!   trimmed, with `brk`. For scale: that session's compositor, unlocked,
//!   had peaked at 284,988 kB resident in all (anonymous memory, files and
//!   shared memory) and held 122,524 kB when both were read.
//!
//!   A `heap-debug` build (`shojiwm_lib/heap-debug`, which
//!   `dist/install.sh --rust-config --debug` installs) does not pin, and
//!   says so in one line: [`shojiwm_lib::HEAP_DEBUG`] is read before the
//!   limit. Such a build has mimalloc's assertions on, and there the refused
//!   call fails one. The mimalloc test program built that way aborted at the
//!   first purge, about 1 s after the block was freed (`mimalloc: assertion
//!   failed: ... mi_os_decommit_ex assertion: "err == 0"` on stderr).
//!
//! # Checking it, switching it off
//!
//! One line in the session log says what happened. `Locked:` in
//! `/proc/<pid>/smaps_rollup` is the RAM held, shared pages counted by their
//! share. `VmLck` in `/proc/<pid>/status` is the address space of every
//! lockable mapping, touched or not: expect about the 2.4 GB the compositor
//! maps, not the RAM held. `SHOJI_PIN_MEMORY` set to `off`, `0` or `false`
//! turns the feature off, and nothing in the environment turns it on.

use std::fmt;

use crate::watchdog::env_says_off;

/// Set to `off`, `0` or `false`, it leaves memory unpinned whatever the
/// config asks for. Like `SHOJI_RUNTIME_WATCHDOG` it only turns the feature
/// off.
const OFF_VARIABLE: &str = "SHOJI_PIN_MEMORY";

/// Every mapping the process has and every one it makes later, each page
/// locked when it is first touched. The only combination ever passed.
const FLAGS: libc::c_int = libc::MCL_CURRENT | libc::MCL_FUTURE | libc::MCL_ONFAULT;

/// An errno.
type Errno = libc::c_int;

/// `RLIMIT_MEMLOCK`, in bytes of address space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Limit {
    soft: libc::rlim_t,
    hard: libc::rlim_t,
}

/// A limit as `prlimit` shows it.
struct Bytes(libc::rlim_t);

impl fmt::Display for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            libc::RLIM_INFINITY => f.write_str("unlimited"),
            bytes => write!(f, "{bytes} bytes"),
        }
    }
}

/// What a limit calls for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// The soft limit is unlimited.
    Lock,
    /// Only the hard limit is unlimited: the soft limit can be raised to it.
    Raise,
    /// The hard limit is finite, so no raise makes the soft limit unlimited.
    Leave,
}

fn decide(soft: libc::rlim_t, hard: libc::rlim_t) -> Step {
    if soft == libc::RLIM_INFINITY {
        Step::Lock
    } else if hard == libc::RLIM_INFINITY {
        Step::Raise
    } else {
        Step::Leave
    }
}

/// The three system calls, so the tests can answer them.
trait Kernel {
    fn getrlimit(&mut self) -> Result<Limit, Errno>;
    fn setrlimit(&mut self, limit: Limit) -> Result<(), Errno>;
    fn mlockall(&mut self, flags: libc::c_int) -> Result<(), Errno>;
}

/// The running kernel.
struct Linux;

/// A system call's 0, or the errno behind its -1.
fn checked(result: libc::c_int) -> Result<(), Errno> {
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }
}

impl Kernel for Linux {
    fn getrlimit(&mut self) -> Result<Limit, Errno> {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit gets a valid pointer to an rlimit.
        checked(unsafe { libc::getrlimit(libc::RLIMIT_MEMLOCK, &mut limit) })?;
        Ok(Limit {
            soft: limit.rlim_cur,
            hard: limit.rlim_max,
        })
    }

    fn setrlimit(&mut self, limit: Limit) -> Result<(), Errno> {
        let limit = libc::rlimit {
            rlim_cur: limit.soft,
            rlim_max: limit.hard,
        };
        // SAFETY: setrlimit gets a valid pointer to an rlimit.
        checked(unsafe { libc::setrlimit(libc::RLIMIT_MEMLOCK, &limit) })
    }

    fn mlockall(&mut self, flags: libc::c_int) -> Result<(), Errno> {
        // SAFETY: mlockall takes no pointer. It changes where this process's
        // pages may be kept, not what they hold.
        checked(unsafe { libc::mlockall(flags) })
    }
}

/// How an attempt ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// The option is off: nothing was looked at.
    Off,
    /// [`OFF_VARIABLE`] says off: no system call was made.
    OffByEnvironment,
    /// A `heap-debug` build, where mimalloc aborts when it purges freed
    /// memory that is locked: no system call was made.
    OffForHeapDebug,
    /// Locked. `raised`: the soft limit was raised to the hard one first.
    Pinned { raised: bool },
    /// The hard limit is finite: nothing was changed.
    LimitFinite(Limit),
    /// `getrlimit` failed.
    LimitUnread(Errno),
    /// `setrlimit` would not raise the soft limit to the hard one.
    RaiseRefused(Errno, Limit),
    /// `setrlimit` returned 0, and the soft limit read back is still finite.
    RaiseIgnored(Limit),
    /// `mlockall` failed although the soft limit is unlimited.
    LockFailed { errno: Errno, raised: bool },
}

/// The whole sequence. `variable` is the value of [`OFF_VARIABLE`], and
/// `heap_debug` says whether this is a `heap-debug` build
/// ([`shojiwm_lib::HEAP_DEBUG`]).
fn attempt(
    enabled: bool,
    variable: Option<&str>,
    heap_debug: bool,
    kernel: &mut impl Kernel,
) -> Outcome {
    if !enabled {
        return Outcome::Off;
    }
    if env_says_off(variable) {
        return Outcome::OffByEnvironment;
    }
    if heap_debug {
        return Outcome::OffForHeapDebug;
    }
    let mut limit = match kernel.getrlimit() {
        Ok(limit) => limit,
        Err(errno) => return Outcome::LimitUnread(errno),
    };
    let mut raised = false;
    if decide(limit.soft, limit.hard) == Step::Raise {
        let wanted = Limit {
            soft: limit.hard,
            hard: limit.hard,
        };
        if let Err(errno) = kernel.setrlimit(wanted) {
            return Outcome::RaiseRefused(errno, limit);
        }
        // Read again: what is locked next depends on the limit in force, not
        // on the one asked for.
        limit = match kernel.getrlimit() {
            Ok(limit) => limit,
            Err(errno) => return Outcome::LimitUnread(errno),
        };
        raised = true;
    }
    match decide(limit.soft, limit.hard) {
        Step::Lock => match kernel.mlockall(FLAGS) {
            Ok(()) => Outcome::Pinned { raised },
            Err(errno) => Outcome::LockFailed { errno, raised },
        },
        // Only after a raise: one that returned 0 and moved nothing.
        Step::Raise => Outcome::RaiseIgnored(limit),
        Step::Leave => Outcome::LimitFinite(limit),
    }
}

impl Outcome {
    /// One line, written from this crate: the session log's default filter
    /// keeps `shojiwm_rs` at info and drops a config crate's info lines.
    fn log(self) {
        let error = std::io::Error::from_raw_os_error;
        match self {
            Self::Off => {}
            Self::OffByEnvironment => {
                tracing::info!("memory not pinned ({OFF_VARIABLE}=off)");
            }
            Self::OffForHeapDebug => {
                tracing::info!("memory not pinned: this is a heap-debug build");
            }
            Self::Pinned { raised } => tracing::info!(
                soft_limit_raised = raised,
                "memory pinned: a page stays in RAM once it is touched"
            ),
            Self::LimitFinite(limit) => tracing::info!(
                soft = %Bytes(limit.soft),
                hard = %Bytes(limit.hard),
                remedy = "a line `<user> hard memlock unlimited` in a file under \
                          /etc/security/limits.d/, then a new login",
                "memory not pinned: the memlock limit is finite"
            ),
            Self::LimitUnread(errno) => tracing::warn!(
                error = %error(errno),
                "memory not pinned: the memlock limit could not be read"
            ),
            Self::RaiseRefused(errno, limit) => tracing::warn!(
                error = %error(errno),
                soft = %Bytes(limit.soft),
                hard = %Bytes(limit.hard),
                "memory not pinned: the soft memlock limit could not be raised to the hard limit"
            ),
            Self::RaiseIgnored(limit) => tracing::warn!(
                soft = %Bytes(limit.soft),
                hard = %Bytes(limit.hard),
                "memory not pinned: setrlimit returned 0 and the soft memlock limit is still finite"
            ),
            Self::LockFailed { errno, raised } => tracing::warn!(
                error = %error(errno),
                soft_limit_raised = raised,
                "memory not pinned: mlockall failed"
            ),
        }
    }
}

/// The sequence on the running kernel, with what the environment says.
/// `heap_debug` is passed in, so a test does not depend on how its binary
/// was built.
fn run(enabled: bool, heap_debug: bool) -> Outcome {
    // The environment is read only when the config asks to pin.
    let variable = if enabled {
        std::env::var(OFF_VARIABLE).ok()
    } else {
        None
    };
    attempt(enabled, variable.as_deref(), heap_debug, &mut Linux)
}

/// Pin the process's memory if `enabled`, and log what happened. First in a
/// launcher's `launch`: once, on the compositor thread, with the log up,
/// before the config's setup and about 2 s before the first GPU is opened.
/// With `enabled` false it makes no system call and writes no line. In a
/// `heap-debug` build ([`shojiwm_lib::HEAP_DEBUG`]) it makes no system call
/// either, and with `enabled` true its line says so.
pub(crate) fn apply(enabled: bool) {
    run(enabled, shojiwm_lib::HEAP_DEBUG).log();
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering::Relaxed},
        },
        thread,
    };

    use super::*;
    use crate::watchdog::run_test_in_child;

    const MIB: libc::rlim_t = 1 << 20;
    const UNLIMITED: libc::rlim_t = libc::RLIM_INFINITY;
    /// systemd's default for a login, and what the finite child test runs under.
    const LOGIN_DEFAULT: libc::rlim_t = 8 * MIB;
    const ONFAULT: libc::c_int = libc::MCL_CURRENT | libc::MCL_FUTURE | libc::MCL_ONFAULT;

    fn limit(soft: libc::rlim_t, hard: libc::rlim_t) -> Limit {
        Limit { soft, hard }
    }

    // -- pure ---------------------------------------------------------------

    #[test]
    fn an_unlimited_soft_limit_is_locked() {
        assert_eq!(decide(UNLIMITED, UNLIMITED), Step::Lock);
    }

    #[test]
    fn a_finite_soft_limit_under_an_unlimited_hard_limit_is_raised() {
        for soft in [0, LOGIN_DEFAULT, UNLIMITED - 1] {
            assert_eq!(decide(soft, UNLIMITED), Step::Raise, "{soft}");
        }
    }

    #[test]
    fn a_finite_hard_limit_is_left_alone_however_large() {
        // The last two: PipeWire's packaged 4 GiB, and one byte short of unlimited.
        for (soft, hard) in [
            (0, 0),
            (LOGIN_DEFAULT, LOGIN_DEFAULT),
            (LOGIN_DEFAULT, 64 * MIB),
            (4096 * MIB, 4096 * MIB),
            (UNLIMITED - 1, UNLIMITED - 1),
        ] {
            assert_eq!(decide(soft, hard), Step::Leave, "{soft} {hard}");
        }
    }

    #[test]
    fn limits_read_as_prlimit_shows_them() {
        assert_eq!(Bytes(LOGIN_DEFAULT).to_string(), "8388608 bytes");
        assert_eq!(Bytes(UNLIMITED).to_string(), "unlimited");
    }

    // -- the sequence, over a fake kernel -------------------------------------

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Call {
        Getrlimit,
        Setrlimit(Limit),
        Mlockall(libc::c_int),
    }

    /// The three calls, answered by the rules measured on Linux 7.3.6 on
    /// 27/11/2026, all under a finite limit: an unlimited one could not be
    /// had there. For an unlimited limit the same rules say the raise is
    /// taken and the lock succeeds, as getrlimit(2) and mm/mlock.c read; the
    /// unlimited child test below asks the running kernel once the hard limit
    /// allows. The `Option`s and `setrlimit_ignored` are answers the kernel
    /// itself did not give. A seccomp filter gave each of them when tried
    /// (`SECCOMP_RET_ERRNO`), and they let the four failure outcomes be
    /// tested.
    struct Fake {
        limit: Limit,
        /// Lockable address space mapped: what a finite limit is charged.
        /// The compositor's, as measured.
        mapped: libc::rlim_t,
        /// The flags in force.
        locked: Option<libc::c_int>,
        calls: Vec<Call>,
        getrlimit_fails: Option<Errno>,
        setrlimit_fails: Option<Errno>,
        setrlimit_ignored: bool,
        mlockall_fails: Option<Errno>,
    }

    impl Fake {
        fn new(soft: libc::rlim_t, hard: libc::rlim_t) -> Self {
            Self {
                limit: limit(soft, hard),
                mapped: 2_408_952 << 10,
                locked: None,
                calls: Vec::new(),
                getrlimit_fails: None,
                setrlimit_fails: None,
                setrlimit_ignored: false,
                mlockall_fails: None,
            }
        }
    }

    impl Kernel for Fake {
        fn getrlimit(&mut self) -> Result<Limit, Errno> {
            self.calls.push(Call::Getrlimit);
            self.getrlimit_fails.map_or(Ok(self.limit), Err)
        }

        fn setrlimit(&mut self, limit: Limit) -> Result<(), Errno> {
            self.calls.push(Call::Setrlimit(limit));
            if let Some(errno) = self.setrlimit_fails {
                return Err(errno);
            }
            // Measured: soft above hard is EINVAL, a higher hard limit is
            // EPERM for a user, and soft up to hard is taken.
            if limit.soft > limit.hard {
                return Err(libc::EINVAL);
            }
            if limit.hard > self.limit.hard {
                return Err(libc::EPERM);
            }
            if !self.setrlimit_ignored {
                self.limit = limit;
            }
            Ok(())
        }

        fn mlockall(&mut self, flags: libc::c_int) -> Result<(), Errno> {
            self.calls.push(Call::Mlockall(flags));
            if let Some(errno) = self.mlockall_fails {
                return Err(errno);
            }
            // Measured: a soft limit of 0 is EPERM; with MCL_CURRENT, more
            // mapped than the soft limit is ENOMEM and nothing is locked;
            // MCL_FUTURE alone is taken at any size.
            if self.limit.soft == 0 {
                return Err(libc::EPERM);
            }
            if flags & libc::MCL_CURRENT != 0 && self.mapped > self.limit.soft {
                return Err(libc::ENOMEM);
            }
            self.locked = Some(flags);
            Ok(())
        }
    }

    #[test]
    fn off_makes_no_call() {
        // Nothing in the environment turns it on, and neither does the build.
        for variable in [None, Some("1"), Some("on"), Some("off")] {
            for heap_debug in [false, true] {
                let mut kernel = Fake::new(UNLIMITED, UNLIMITED);
                assert_eq!(
                    attempt(false, variable, heap_debug, &mut kernel),
                    Outcome::Off
                );
                assert!(kernel.calls.is_empty(), "{:?}", kernel.calls);
            }
        }
    }

    #[test]
    fn the_environment_turns_it_off_before_any_call() {
        // In a heap-debug build too: the variable is looked at first.
        for variable in ["off", "0", "false"] {
            for heap_debug in [false, true] {
                let mut kernel = Fake::new(UNLIMITED, UNLIMITED);
                assert_eq!(
                    attempt(true, Some(variable), heap_debug, &mut kernel),
                    Outcome::OffByEnvironment
                );
                assert!(kernel.calls.is_empty(), "{:?}", kernel.calls);
            }
        }
        for variable in [None, Some("1"), Some("on"), Some("")] {
            let mut kernel = Fake::new(UNLIMITED, UNLIMITED);
            assert_eq!(
                attempt(true, variable, false, &mut kernel),
                Outcome::Pinned { raised: false }
            );
        }
    }

    #[test]
    fn a_heap_debug_build_makes_no_call_whatever_the_limit() {
        // The first two limits would be locked without the flag, the last
        // only read.
        for (soft, hard) in [
            (UNLIMITED, UNLIMITED),
            (LOGIN_DEFAULT, UNLIMITED),
            (LOGIN_DEFAULT, LOGIN_DEFAULT),
        ] {
            // Nothing in the environment turns it back on.
            for variable in [None, Some("1"), Some("on"), Some("")] {
                let mut kernel = Fake::new(soft, hard);
                assert_eq!(
                    attempt(true, variable, true, &mut kernel),
                    Outcome::OffForHeapDebug,
                    "{soft} {hard} {variable:?}"
                );
                assert!(kernel.calls.is_empty(), "{:?}", kernel.calls);
                assert_eq!(kernel.limit, limit(soft, hard));
                assert_eq!(kernel.locked, None);
            }
        }
    }

    #[test]
    fn an_unlimited_limit_is_locked_as_it_stands() {
        let mut kernel = Fake::new(UNLIMITED, UNLIMITED);
        assert_eq!(
            attempt(true, None, false, &mut kernel),
            Outcome::Pinned { raised: false }
        );
        assert_eq!(kernel.calls, [Call::Getrlimit, Call::Mlockall(ONFAULT)]);
        assert_eq!(kernel.locked, Some(ONFAULT));
    }

    #[test]
    fn a_soft_limit_is_raised_to_an_unlimited_hard_limit_read_again_then_locked() {
        let mut kernel = Fake::new(LOGIN_DEFAULT, UNLIMITED);
        assert_eq!(
            attempt(true, None, false, &mut kernel),
            Outcome::Pinned { raised: true }
        );
        assert_eq!(
            kernel.calls,
            [
                Call::Getrlimit,
                Call::Setrlimit(limit(UNLIMITED, UNLIMITED)),
                Call::Getrlimit,
                Call::Mlockall(ONFAULT),
            ]
        );
        assert_eq!(kernel.locked, Some(ONFAULT));
        // Not lowered again: nothing follows the lock.
        assert_eq!(kernel.limit, limit(UNLIMITED, UNLIMITED));
    }

    #[test]
    fn a_refused_raise_locks_nothing() {
        let mut kernel = Fake::new(LOGIN_DEFAULT, UNLIMITED);
        kernel.setrlimit_fails = Some(libc::EPERM);
        assert_eq!(
            attempt(true, None, false, &mut kernel),
            Outcome::RaiseRefused(libc::EPERM, limit(LOGIN_DEFAULT, UNLIMITED))
        );
        assert_eq!(
            kernel.calls,
            [
                Call::Getrlimit,
                Call::Setrlimit(limit(UNLIMITED, UNLIMITED))
            ]
        );
        assert_eq!(kernel.locked, None);
    }

    #[test]
    fn a_limit_still_finite_after_the_raise_locks_nothing() {
        let mut kernel = Fake::new(LOGIN_DEFAULT, UNLIMITED);
        kernel.setrlimit_ignored = true;
        assert_eq!(
            attempt(true, None, false, &mut kernel),
            Outcome::RaiseIgnored(limit(LOGIN_DEFAULT, UNLIMITED))
        );
        assert_eq!(
            kernel.calls,
            [
                Call::Getrlimit,
                Call::Setrlimit(limit(UNLIMITED, UNLIMITED)),
                Call::Getrlimit,
            ]
        );
        assert_eq!(kernel.locked, None);
    }

    #[test]
    fn a_finite_hard_limit_is_only_read() {
        for (soft, hard) in [
            (LOGIN_DEFAULT, LOGIN_DEFAULT),
            (LOGIN_DEFAULT, 64 * MIB),
            (0, 0),
            (4096 * MIB, 4096 * MIB),
        ] {
            let mut kernel = Fake::new(soft, hard);
            assert_eq!(
                attempt(true, None, false, &mut kernel),
                Outcome::LimitFinite(limit(soft, hard))
            );
            // No setrlimit and no mlockall.
            assert_eq!(kernel.calls, [Call::Getrlimit], "{soft} {hard}");
            assert_eq!(kernel.limit, limit(soft, hard));
            assert_eq!(kernel.locked, None);
        }
    }

    #[test]
    fn a_failed_lock_is_reported_with_its_errno() {
        for (soft, raised) in [(UNLIMITED, false), (LOGIN_DEFAULT, true)] {
            let mut kernel = Fake::new(soft, UNLIMITED);
            kernel.mlockall_fails = Some(libc::ENOMEM);
            assert_eq!(
                attempt(true, None, false, &mut kernel),
                Outcome::LockFailed {
                    errno: libc::ENOMEM,
                    raised
                }
            );
            // One call, with the one set of flags: no second try.
            assert_eq!(kernel.calls.last(), Some(&Call::Mlockall(ONFAULT)));
            assert_eq!(
                kernel
                    .calls
                    .iter()
                    .filter(|call| matches!(call, Call::Mlockall(_)))
                    .count(),
                1
            );
            assert_eq!(kernel.locked, None);
        }
    }

    #[test]
    fn an_unreadable_limit_locks_nothing() {
        let mut kernel = Fake::new(UNLIMITED, UNLIMITED);
        kernel.getrlimit_fails = Some(libc::EPERM);
        assert_eq!(
            attempt(true, None, false, &mut kernel),
            Outcome::LimitUnread(libc::EPERM)
        );
        assert_eq!(kernel.calls, [Call::Getrlimit]);
    }

    // -- the line in the log --------------------------------------------------

    /// Counts the lines the session log keeps: its default filter holds
    /// `shojiwm_rs` at info.
    struct Kept(Arc<AtomicUsize>);

    impl tracing::Subscriber for Kept {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::INFO
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, _: &tracing::Event<'_>) {
            self.0.fetch_add(1, Relaxed);
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn off_writes_no_line_and_every_other_outcome_writes_one() {
        let read = limit(LOGIN_DEFAULT, UNLIMITED);
        let kept = Arc::new(AtomicUsize::new(0));
        tracing::subscriber::with_default(Kept(kept.clone()), || {
            Outcome::Off.log();
            assert_eq!(kept.load(Relaxed), 0, "off wrote a line");
            let others = [
                Outcome::OffByEnvironment,
                Outcome::OffForHeapDebug,
                Outcome::Pinned { raised: true },
                Outcome::LimitFinite(read),
                Outcome::LimitUnread(libc::EPERM),
                Outcome::RaiseRefused(libc::EPERM, read),
                Outcome::RaiseIgnored(read),
                Outcome::LockFailed {
                    errno: libc::ENOMEM,
                    raised: false,
                },
            ];
            for (written, outcome) in others.into_iter().enumerate() {
                outcome.log();
                assert_eq!(kept.load(Relaxed), written + 1, "{outcome:?}");
            }
        });
    }

    // -- the real calls, in a child process -----------------------------------

    const CHILD: &str = "SHOJIWM_MEMORY_PIN_CHILD";

    /// Re-run test `name` alone in a child with `scenario`, and with
    /// [`OFF_VARIABLE`] set to `variable` or absent. The harness itself never
    /// locks anything. The child has to pass and to have run its scenario:
    /// libtest also exits 0 when its filter matches no test.
    fn passes_in_child(name: &str, scenario: &str, variable: Option<&str>) {
        let (status, stderr, _) = run_test_in_child(
            module_path!(),
            name,
            &[
                (CHILD, Some(OsStr::new(scenario))),
                (OFF_VARIABLE, variable.map(OsStr::new)),
            ],
        );
        // The child's own lines, for a run with `--nocapture`.
        for line in stderr.lines().filter(|line| line.starts_with("child: ")) {
            eprintln!("memory_pin {line}");
        }
        assert!(status.success(), "{status:?} {stderr}");
        assert!(
            stderr.contains(&format!("child: {scenario}: ")),
            "the child ran no test named {name}: {stderr}"
        );
    }

    /// A size in kB from this process's `/proc/self/status`.
    fn status_kb(name: &str) -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse().ok())
            .unwrap_or_else(|| panic!("no {name} in /proc/self/status"))
    }

    /// `VmLck`: the address space under a lock, touched or not.
    fn vm_lck() -> u64 {
        status_kb("VmLck")
    }

    /// Map `bytes` of anonymous memory, write to every page and unmap it:
    /// what a new allocator arena, a client's pool or a thread stack asks of
    /// the kernel.
    fn map_and_write(bytes: usize) -> std::io::Result<()> {
        // SAFETY: a new private anonymous mapping, written to inside its
        // length and unmapped before it is forgotten.
        unsafe {
            let start = libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            if start == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            let first = start.cast::<u8>();
            for offset in (0..bytes).step_by(4096) {
                first.add(offset).write_volatile(1);
            }
            libc::munmap(start, bytes);
        }
        Ok(())
    }

    /// What a finite limit under `MCL_FUTURE` breaks: a mapping, a heap
    /// allocation and a new thread, each used.
    fn allocations_still_work(bytes: usize) {
        map_and_write(bytes).expect("an anonymous mapping failed");
        let block = std::hint::black_box(vec![1u8; bytes]);
        assert_eq!(block[bytes - 1], 1);
        drop(block);
        let spawned = thread::Builder::new()
            .spawn(|| std::hint::black_box(7))
            .expect("a thread could not be spawned");
        assert_eq!(spawned.join().unwrap(), 7);
    }

    /// In the child: the real sequence on the real kernel.
    fn child_pins(scenario: &str) {
        // SAFETY: prctl(PR_SET_DUMPABLE) only changes this process's flag.
        unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
        let mut kernel = Linux;
        let inherited = kernel.getrlimit().unwrap();
        // A process may always lower its own limits. The unlimited case keeps
        // its hard limit and starts from a finite soft limit, as the compositor
        // does at a login: wherever the test is started, the real raise runs.
        let hard = if scenario == "unlimited" {
            inherited.hard
        } else {
            inherited.hard.min(LOGIN_DEFAULT)
        };
        let lowered = limit(inherited.soft.min(LOGIN_DEFAULT), hard);
        if lowered != inherited {
            kernel.setrlimit(lowered).unwrap();
        }
        let before = kernel.getrlimit().unwrap();
        if scenario == "refused" {
            return child_is_refused(before);
        }
        // What `apply` does in a build without heap-debug, however this test
        // binary was built: the read of the environment, then the line.
        let outcome = run(true, false);
        outcome.log();
        let (after, locked) = (kernel.getrlimit().unwrap(), vm_lck());
        eprintln!(
            "child: {scenario}: limit {} / {} before, {} / {} after, {outcome:?}, VmLck {locked} kB",
            Bytes(before.soft),
            Bytes(before.hard),
            Bytes(after.soft),
            Bytes(after.hard),
        );
        match scenario {
            "finite" | "off" => {
                // "off" has the variable in its environment. Under this limit
                // a `run` that did not read it would end as "finite" does.
                let expected = if scenario == "off" {
                    Outcome::OffByEnvironment
                } else {
                    Outcome::LimitFinite(before)
                };
                assert_eq!(outcome, expected);
                assert_eq!(after, before);
                assert_eq!(locked, 0);
                allocations_still_work(16 << 20);
            }
            "unlimited" => {
                assert_eq!(outcome, Outcome::Pinned { raised: true });
                // Raised, and not lowered again.
                assert_eq!(after, limit(UNLIMITED, UNLIMITED));
                assert!(locked > 0);
                allocations_still_work(64 << 20);
            }
            other => panic!("unknown scenario {other}"),
        }
        eprintln!(
            "child: {scenario}: mapping, allocation and thread worked, VmLck {} kB",
            vm_lck()
        );
    }

    /// In the child, under a finite limit: the call `attempt` never makes
    /// there, to see what the running kernel leaves behind when it says no.
    fn child_is_refused(limit: Limit) {
        let mapped = status_kb("VmSize");
        let answer = Linux.mlockall(FLAGS);
        let locked = vm_lck();
        eprintln!(
            "child: refused: {mapped} kB mapped, soft limit {}, mlockall = {}, VmLck {locked} kB",
            Bytes(limit.soft),
            match answer {
                Ok(()) => "0".to_owned(),
                Err(errno) => std::io::Error::from_raw_os_error(errno).to_string(),
            }
        );
        if answer.is_ok() {
            // Smaller than its limit, or privileged: nothing was refused. A
            // lock that was taken shows in VmLck at once. MCL_FUTURE without
            // MCL_CURRENT is taken under any limit, and shows nothing.
            assert!(locked > 0, "mlockall returned 0 and VmLck is 0 kB");
            eprintln!("child: refused: the kernel took the lock, so there is nothing to check");
            return;
        }
        // Measured: ENOMEM with more mapped than the limit, EPERM at a limit of 0.
        assert!(
            matches!(answer, Err(libc::ENOMEM | libc::EPERM)),
            "{answer:?}"
        );
        assert_eq!(locked, 0);
        // MCL_FUTURE was not left on either.
        allocations_still_work(16 << 20);
        assert_eq!(vm_lck(), 0);
        eprintln!("child: refused: mapping, allocation and thread worked, VmLck 0 kB");
    }

    #[test]
    fn a_lock_the_real_kernel_refuses_leaves_nothing_locked() {
        if let Ok(scenario) = std::env::var(CHILD) {
            return child_pins(&scenario);
        }
        passes_in_child(
            "a_lock_the_real_kernel_refuses_leaves_nothing_locked",
            "refused",
            None,
        );
    }

    #[test]
    fn a_finite_limit_leaves_the_real_process_unlocked_and_working() {
        if let Ok(scenario) = std::env::var(CHILD) {
            return child_pins(&scenario);
        }
        passes_in_child(
            "a_finite_limit_leaves_the_real_process_unlocked_and_working",
            "finite",
            None,
        );
    }

    #[test]
    fn the_variable_switches_the_real_path_off() {
        if let Ok(scenario) = std::env::var(CHILD) {
            return child_pins(&scenario);
        }
        passes_in_child(
            "the_variable_switches_the_real_path_off",
            "off",
            Some("off"),
        );
    }

    #[test]
    fn an_unlimited_limit_pins_the_real_process_and_it_keeps_working() {
        if let Ok(scenario) = std::env::var(CHILD) {
            return child_pins(&scenario);
        }
        // Read only: a child does the locking.
        let inherited = Linux.getrlimit().unwrap();
        if inherited.hard != UNLIMITED {
            eprintln!(
                "memory_pin: skipped the unlimited case: the hard memlock limit here is {} \
                 (it runs once `<user> hard memlock unlimited` is in a file under \
                 /etc/security/limits.d/ and that user has logged in again)",
                Bytes(inherited.hard)
            );
            return;
        }
        passes_in_child(
            "an_unlimited_limit_pins_the_real_process_and_it_keeps_working",
            "unlimited",
            None,
        );
    }
}
