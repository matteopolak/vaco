//! A narrow safe wrapper around Linux `perf_event_open` CPU-cycle counters.
//!
//! [`CpuCycles::open_for_current_thread`] opens one disabled, pinned
//! `PERF_COUNT_HW_CPU_CYCLES` event for the calling thread. [`CpuCycles::measure`]
//! resets, enables, runs one closure, disables, and reads it. It rejects a
//! multiplexed count instead of scaling it, so successful values are direct
//! hardware-cycle observations rather than estimates.
//!
//! The Linux UAPI comes from `linux-perf-event-open(2)` in
//! `provenance/sources.toml`. The few unsafe calls are confined here because
//! `perf_event_open` has no libc wrapper; callers get no raw descriptor or
//! UAPI structure.

use std::fmt;
use std::io;

/// A direct CPU-cycle count cannot be obtained on this target or from Linux.
#[derive(Debug)]
pub enum CounterError {
    /// Linux `perf_event_open` is unavailable on this target or architecture.
    UnsupportedTarget,
    /// Opening the event failed, commonly because the PMU is unavailable or
    /// `perf_event_paranoid` denies unprivileged access.
    Open(io::Error),
    /// Resetting, enabling, disabling, or reading the event failed.
    Operation {
        operation: &'static str,
        source: io::Error,
    },
    /// Linux returned a partial perf read instead of the requested three words.
    ShortRead(isize),
    /// The PMU counter never ran during the requested measurement interval.
    NotRunning,
    /// The PMU multiplexed the event. Scaling would create an estimate, so the
    /// caller must fall back to a different metric instead.
    Multiplexed { enabled: u64, running: u64 },
}

impl fmt::Display for CounterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTarget => formatter.write_str(
                "Linux perf_event CPU cycles are supported only on x86_64 and aarch64 Linux",
            ),
            Self::Open(source) => write!(formatter, "perf_event_open failed: {source}"),
            Self::Operation { operation, source } => {
                write!(formatter, "perf event {operation} failed: {source}")
            }
            Self::ShortRead(bytes) => write!(formatter, "perf event read returned {bytes} bytes"),
            Self::NotRunning => formatter.write_str("perf event did not run on the PMU"),
            Self::Multiplexed { enabled, running } => write!(
                formatter,
                "perf event was multiplexed (enabled {enabled} ns, running {running} ns)"
            ),
        }
    }
}

impl std::error::Error for CounterError {}

/// A persistent, per-thread Linux CPU-cycle counter.
#[derive(Debug)]
pub struct CpuCycles {
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    fd: std::os::fd::OwnedFd,
}

impl CpuCycles {
    /// Open a disabled pinned CPU-cycle event for the calling thread.
    ///
    /// # Errors
    ///
    /// Returns [`CounterError::UnsupportedTarget`] off Linux x86_64/aarch64,
    /// or [`CounterError::Open`] when Linux refuses the event.
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub fn open_for_current_thread() -> Result<Self, CounterError> {
        linux::open().map(|fd| Self { fd })
    }

    /// Report that this target has no supported Linux perf-event ABI.
    ///
    /// # Errors
    ///
    /// Always returns [`CounterError::UnsupportedTarget`].
    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    pub fn open_for_current_thread() -> Result<Self, CounterError> {
        Err(CounterError::UnsupportedTarget)
    }

    /// Measure one closure and return its unmultiplexed hardware CPU cycles.
    ///
    /// Reset/enable and disable/read are outside the closure. The closure must
    /// not move this counter to another thread; `CpuCycles` is deliberately
    /// neither exposed as a raw descriptor nor shared by the checkasm runner.
    ///
    /// # Errors
    ///
    /// Returns the Linux operation error, [`CounterError::NotRunning`], or
    /// [`CounterError::Multiplexed`] rather than fabricating a cycle total.
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    pub fn measure<T>(&mut self, work: impl FnOnce() -> T) -> Result<(T, u64), CounterError> {
        linux::measure(&self.fd, work)
    }

    /// This constructor never succeeds outside the supported Linux targets.
    ///
    /// # Errors
    /// Always returns [`CounterError::UnsupportedTarget`].
    #[cfg(not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )))]
    pub fn measure<T>(&mut self, _work: impl FnOnce() -> T) -> Result<(T, u64), CounterError> {
        Err(CounterError::UnsupportedTarget)
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod linux {
    use std::mem::{offset_of, size_of};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::raw::{c_int, c_long, c_ulong, c_void};

    use super::CounterError;

    const PERF_TYPE_HARDWARE: u32 = 0;
    const PERF_COUNT_HW_CPU_CYCLES: u64 = 0;
    const PERF_FORMAT_TOTAL_TIME_ENABLED: u64 = 1;
    const PERF_FORMAT_TOTAL_TIME_RUNNING: u64 = 2;
    const ATTR_DISABLED: u64 = 1;
    const ATTR_PINNED: u64 = 1 << 2;
    const ATTR_EXCLUDE_KERNEL: u64 = 1 << 5;
    const ATTR_EXCLUDE_HYPERVISOR: u64 = 1 << 6;
    const PERF_EVENT_IOC_ENABLE: c_ulong = 0x2400;
    const PERF_EVENT_IOC_DISABLE: c_ulong = 0x2401;
    const PERF_EVENT_IOC_RESET: c_ulong = 0x2403;
    const PERF_EVENT_OPEN_NR: c_long = {
        #[cfg(target_arch = "x86_64")]
        {
            298
        }
        #[cfg(target_arch = "aarch64")]
        {
            241
        }
    };

    /// `perf_event_attr` through `sig_data`, matching the Linux UAPI's 128-byte
    /// layout. Unused union fields are represented by their equally sized word.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PerfEventAttr {
        type_: u32,
        size: u32,
        config: u64,
        sample_period: u64,
        sample_type: u64,
        read_format: u64,
        flags: u64,
        wakeup_events: u32,
        bp_type: u32,
        config1: u64,
        config2: u64,
        branch_sample_type: u64,
        sample_regs_user: u64,
        sample_stack_user: u32,
        clockid: i32,
        sample_regs_intr: u64,
        aux_watermark: u32,
        sample_max_stack: u16,
        reserved_2: u16,
        aux_sample_size: u32,
        reserved_3: u32,
        sig_data: u64,
    }

    impl PerfEventAttr {
        const fn cpu_cycles() -> Self {
            Self {
                type_: PERF_TYPE_HARDWARE,
                size: size_of::<Self>() as u32,
                config: PERF_COUNT_HW_CPU_CYCLES,
                sample_period: 0,
                sample_type: 0,
                read_format: PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING,
                flags: ATTR_DISABLED | ATTR_PINNED | ATTR_EXCLUDE_KERNEL | ATTR_EXCLUDE_HYPERVISOR,
                wakeup_events: 0,
                bp_type: 0,
                config1: 0,
                config2: 0,
                branch_sample_type: 0,
                sample_regs_user: 0,
                sample_stack_user: 0,
                clockid: 0,
                sample_regs_intr: 0,
                aux_watermark: 0,
                sample_max_stack: 0,
                reserved_2: 0,
                aux_sample_size: 0,
                reserved_3: 0,
                sig_data: 0,
            }
        }
    }

    const _: () = assert!(size_of::<PerfEventAttr>() == 128);
    const _: () = assert!(offset_of!(PerfEventAttr, flags) == 40);
    const _: () = assert!(offset_of!(PerfEventAttr, sig_data) == 120);

    #[repr(C)]
    #[derive(Default)]
    struct PerfRead {
        value: u64,
        time_enabled: u64,
        time_running: u64,
    }

    unsafe extern "C" {
        fn syscall(number: c_long, ...) -> c_long;
        fn ioctl(fd: c_int, request: c_ulong, argument: c_ulong) -> c_int;
        fn read(fd: c_int, buffer: *mut c_void, count: usize) -> isize;
    }

    pub(super) fn open() -> Result<OwnedFd, CounterError> {
        let attr = PerfEventAttr::cpu_cycles();
        // SAFETY: `attr` has the Linux UAPI layout asserted above and remains
        // valid for the synchronous syscall. pid=0/cpu=-1 selects this thread;
        // group_fd=-1 creates one group leader; flags=0 supplies no extra ABI.
        let fd = unsafe {
            syscall(
                PERF_EVENT_OPEN_NR,
                std::ptr::from_ref(&attr),
                0_i32,
                -1_i32,
                -1_i32,
                0_u64,
            )
        };
        if fd < 0 {
            return Err(CounterError::Open(std::io::Error::last_os_error()));
        }
        let raw_fd = c_int::try_from(fd).map_err(|_| {
            CounterError::Open(std::io::Error::other("perf event descriptor overflow"))
        })?;
        // SAFETY: successful `perf_event_open` returns an owned file descriptor;
        // `OwnedFd` takes responsibility for closing it exactly once.
        Ok(unsafe { OwnedFd::from_raw_fd(raw_fd) })
    }

    pub(super) fn measure<T>(
        fd: &OwnedFd,
        work: impl FnOnce() -> T,
    ) -> Result<(T, u64), CounterError> {
        control(fd, PERF_EVENT_IOC_RESET, "reset")?;
        control(fd, PERF_EVENT_IOC_ENABLE, "enable")?;
        let output = work();
        control(fd, PERF_EVENT_IOC_DISABLE, "disable")?;

        let mut count = PerfRead::default();
        // SAFETY: `count` is a writable `PerfRead` whose exact 24-byte layout
        // matches the selected non-group read format. The descriptor stays open.
        let bytes = unsafe {
            read(
                fd.as_raw_fd(),
                std::ptr::from_mut(&mut count).cast::<c_void>(),
                size_of::<PerfRead>(),
            )
        };
        if bytes < 0 {
            return Err(CounterError::Operation {
                operation: "read",
                source: std::io::Error::last_os_error(),
            });
        }
        if bytes != size_of::<PerfRead>() as isize {
            return Err(CounterError::ShortRead(bytes));
        }
        if count.time_running == 0 {
            return Err(CounterError::NotRunning);
        }
        if count.time_running != count.time_enabled {
            return Err(CounterError::Multiplexed {
                enabled: count.time_enabled,
                running: count.time_running,
            });
        }
        Ok((output, count.value))
    }

    fn control(
        fd: &OwnedFd,
        request: c_ulong,
        operation: &'static str,
    ) -> Result<(), CounterError> {
        // SAFETY: the three requests are `_IO` perf UAPI commands, so a zero
        // third argument is the documented ioctl argument; `fd` is owned/open.
        let result = unsafe { ioctl(fd.as_raw_fd(), request, 0) };
        if result == 0 {
            Ok(())
        } else {
            Err(CounterError::Operation {
                operation,
                source: std::io::Error::last_os_error(),
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use std::mem::{offset_of, size_of};

        use super::PerfEventAttr;

        #[test]
        fn perf_event_attr_matches_the_linux_uapi_layout() {
            assert_eq!(size_of::<PerfEventAttr>(), 128);
            assert_eq!(offset_of!(PerfEventAttr, flags), 40);
            assert_eq!(offset_of!(PerfEventAttr, sig_data), 120);
        }
    }
}

#[cfg(all(
    test,
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))
))]
mod portable_tests {
    use super::{CounterError, CpuCycles};

    #[test]
    fn opening_is_explicitly_unsupported_off_linux_x86_or_arm() {
        assert!(matches!(
            CpuCycles::open_for_current_thread(),
            Err(CounterError::UnsupportedTarget)
        ));
    }
}
