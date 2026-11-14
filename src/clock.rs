//! Opening a PHC, its capabilities, and reading or steering its time.

use crate::error::{Error, Operation};
use crate::ioctl::{Ioctl, IoctlRequest, PlainBytes, RawCaps, SystemIoctl, as_bytes_mut, zeroed};
use std::ffi::c_long;
use std::fmt;
use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::Path;
use std::time::Duration;

/// A point in time on a PHC or system clock.
///
/// The layout matches `struct ptp_clock_time`: the sign of [`ClockTime::secs`]
/// applies to the whole value and [`ClockTime::nanos`] is never negative.
/// Constructors normalize nanoseconds into `0..1_000_000_000`.
///
/// ```
/// use ptp_clock::ClockTime;
///
/// // Nanoseconds carry into seconds.
/// let t = ClockTime::new(1, 1_500_000_000);
/// assert_eq!((t.secs(), t.nanos()), (2, 500_000_000));
///
/// // A negative value keeps a non-negative nanosecond field.
/// let t = ClockTime::from_nanos(-1_250_000_000);
/// assert_eq!((t.secs(), t.nanos()), (-2, 750_000_000));
/// assert_eq!(t.as_nanos(), -1_250_000_000);
/// assert_eq!(t.to_string(), "-1.250000000s");
/// ```
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ClockTime {
    sec: i64,
    nsec: u32,
    /// Reserved in the UAPI; kept so the layout matches `ptp_clock_time`.
    reserved: u32,
}

unsafe impl PlainBytes for ClockTime {}

const _: () = assert!(core::mem::size_of::<ClockTime>() == 16);

impl ClockTime {
    /// Zero seconds past the epoch.
    pub const ZERO: Self = Self {
        sec: 0,
        nsec: 0,
        reserved: 0,
    };

    /// Creates a timestamp from seconds and nanoseconds, carrying nanoseconds
    /// into the seconds field.
    ///
    /// ```
    /// # use ptp_clock::ClockTime;
    /// assert_eq!(ClockTime::new(3, 0).as_nanos(), 3_000_000_000);
    /// ```
    pub const fn new(sec: i64, nsec: u32) -> Self {
        Self {
            sec: sec.saturating_add((nsec / 1_000_000_000) as i64),
            nsec: nsec % 1_000_000_000,
            reserved: 0,
        }
    }

    /// Creates a timestamp from a signed nanosecond count.
    ///
    /// ```
    /// # use ptp_clock::ClockTime;
    /// assert_eq!(ClockTime::from_nanos(1_000_000_001).nanos(), 1);
    /// ```
    pub const fn from_nanos(nanos: i64) -> Self {
        let mut sec = nanos / 1_000_000_000;
        let mut nsec = nanos % 1_000_000_000;
        if nsec < 0 {
            sec -= 1;
            nsec += 1_000_000_000;
        }
        Self {
            sec,
            nsec: nsec as u32,
            reserved: 0,
        }
    }

    /// Creates a timestamp from a [`Duration`] past the epoch.
    ///
    /// ```
    /// # use std::time::Duration;
    /// # use ptp_clock::ClockTime;
    /// assert_eq!(ClockTime::from_duration(Duration::from_millis(2)).nanos(), 2_000_000);
    /// ```
    pub fn from_duration(duration: Duration) -> Self {
        Self {
            sec: duration.as_secs().min(i64::MAX as u64) as i64,
            nsec: duration.subsec_nanos(),
            reserved: 0,
        }
    }

    /// Whole seconds; the sign applies to the whole timestamp.
    pub const fn secs(self) -> i64 {
        self.sec
    }

    /// Nanoseconds within the second, normally `0..1_000_000_000`.
    pub const fn nanos(self) -> u32 {
        self.nsec
    }

    /// The timestamp as a signed nanosecond count.
    ///
    /// ```
    /// # use ptp_clock::ClockTime;
    /// assert_eq!(ClockTime::new(-1, 500_000_000).as_nanos(), -500_000_000);
    /// ```
    pub fn as_nanos(self) -> i128 {
        i128::from(self.sec) * 1_000_000_000 + i128::from(self.nsec)
    }

    /// Whether this is exactly zero.
    pub const fn is_zero(self) -> bool {
        self.sec == 0 && self.nsec == 0
    }

    pub(crate) fn from_timespec(ts: libc::timespec) -> Self {
        Self {
            sec: widen(ts.tv_sec),
            nsec: ts.tv_nsec as u32,
            reserved: 0,
        }
    }

    pub(crate) fn to_timespec(self) -> Option<libc::timespec> {
        Some(libc::timespec {
            tv_sec: libc::time_t::try_from(self.sec).ok()?,
            tv_nsec: self.nsec as c_long,
        })
    }
}

impl From<Duration> for ClockTime {
    fn from(duration: Duration) -> Self {
        Self::from_duration(duration)
    }
}

impl fmt::Display for ClockTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let total = self.as_nanos();
        let abs = total.unsigned_abs();
        let sign = if total < 0 { "-" } else { "" };
        write!(
            f,
            "{sign}{}.{:09}s",
            abs / 1_000_000_000,
            abs % 1_000_000_000
        )
    }
}

/// What a PHC can do, as reported by `PTP_CLOCK_GETCAPS2`.
///
/// Use these fields to gate calls instead of probing with failing ioctls:
/// a channel index is valid only below [`n_ext_ts`](Self::n_ext_ts) or
/// [`n_per_out`](Self::n_per_out), and phase adjustment needs
/// [`adjust_phase`](Self::adjust_phase).
///
/// ```
/// # use ptp_clock::Capabilities;
/// let caps = Capabilities {
///     max_adj: 100_000_000,
///     n_alarm: 0,
///     n_ext_ts: 2,
///     n_per_out: 1,
///     pps: true,
///     n_pins: 3,
///     cross_timestamping: true,
///     adjust_phase: false,
///     max_phase_adj: 0,
/// };
/// assert!(caps.has_ext_ts(1));
/// assert!(!caps.has_ext_ts(2));
/// assert!(!caps.has_adjust_phase());
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Capabilities {
    /// Largest frequency adjustment the clock accepts, in parts per billion.
    pub max_adj: i32,
    /// Number of programmable alarms (not used by this crate).
    pub n_alarm: u32,
    /// Number of external timestamp channels.
    pub n_ext_ts: u32,
    /// Number of periodic output channels.
    pub n_per_out: u32,
    /// Whether the clock can emit a PPS signal.
    pub pps: bool,
    /// Number of configurable pins.
    pub n_pins: u32,
    /// Whether precise system/PHC cross timestamps are available.
    pub cross_timestamping: bool,
    /// Whether the driver supports phase adjustment.
    pub adjust_phase: bool,
    /// Largest phase adjustment, in nanoseconds.
    pub max_phase_adj: i32,
}

impl Capabilities {
    /// Whether external timestamp channel `index` exists.
    pub fn has_ext_ts(&self, index: u32) -> bool {
        index < self.n_ext_ts
    }

    /// Whether periodic output channel `index` exists.
    pub fn has_perout(&self, index: u32) -> bool {
        index < self.n_per_out
    }

    /// Whether the clock has any configurable pin.
    pub fn has_pins(&self) -> bool {
        self.n_pins > 0
    }

    /// Whether the clock can emit a PPS signal.
    pub fn has_pps(&self) -> bool {
        self.pps
    }

    /// Whether precise cross timestamps are available.
    pub fn has_cross_timestamping(&self) -> bool {
        self.cross_timestamping
    }

    /// Whether phase adjustment is supported.
    pub fn has_adjust_phase(&self) -> bool {
        self.adjust_phase
    }

    fn from_raw(raw: &RawCaps) -> Self {
        let count = |value: i32| u32::try_from(value).unwrap_or(0);
        Self {
            max_adj: raw.max_adj,
            n_alarm: count(raw.n_alarm),
            n_ext_ts: count(raw.n_ext_ts),
            n_per_out: count(raw.n_per_out),
            pps: raw.pps != 0,
            n_pins: count(raw.n_pins),
            cross_timestamping: raw.cross_timestamping != 0,
            adjust_phase: raw.adjust_phase != 0,
            max_phase_adj: raw.max_phase_adj,
        }
    }
}

/// An open PTP hardware clock (`/dev/ptpN`).
///
/// The handle owns the file descriptor and closes it on drop. Every method
/// takes `&self`, so a clock can be shared behind an `Arc`; methods that change
/// the clock need the fd to have been opened read-write, which [`PtpClock::open`]
/// tries first.
///
/// ```no_run
/// # fn main() -> Result<(), ptp_clock::Error> {
/// let clock = ptp_clock::PtpClock::open(0)?;
/// let caps = clock.capabilities()?;
/// println!("max_adj={} ppb, ext_ts={}", caps.max_adj, caps.n_ext_ts);
/// println!("now: {}", clock.get_time()?);
/// # Ok(())
/// # }
/// ```
pub struct PtpClock {
    fd: OwnedFd,
    ioctl: Box<dyn Ioctl + Send + Sync>,
    writable: bool,
}

impl fmt::Debug for PtpClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtpClock")
            .field("fd", &self.fd)
            .field("writable", &self.writable)
            .finish_non_exhaustive()
    }
}

impl PtpClock {
    /// Opens `/dev/ptp{index}`.
    ///
    /// The node is opened read-write so the clock can be steered; if that is
    /// denied, the crate falls back to a read-only handle rather than failing,
    /// and [`PtpClock::is_writable`] reports the outcome.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// assert!(clock.is_writable());
    /// # Ok(())
    /// # }
    /// ```
    pub fn open(index: u32) -> Result<Self, Error> {
        Self::open_path(format!("/dev/ptp{index}"))
    }

    /// Opens an arbitrary PTP clock device node.
    ///
    /// This is the escape hatch for udev-renamed nodes, bind mounts, and
    /// namespace paths.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// let clock = ptp_clock::PtpClock::open_path("/dev/ptp0")?;
    /// # let _ = clock;
    /// # Ok(())
    /// # }
    /// ```
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let mut writable = true;
        let file = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                writable = false;
                OpenOptions::new()
                    .read(true)
                    .open(path)
                    .map_err(|err| Error::open(path, err))?
            }
            Err(err) => return Err(Error::open(path, err)),
        };
        let fd: OwnedFd = file.into();
        writable &= fd_is_writable(fd.as_raw_fd());
        Ok(Self {
            fd,
            ioctl: Box::new(SystemIoctl),
            writable,
        })
    }

    /// Builds a clock from an already-open fd and an ioctl dispatcher.
    ///
    /// The fd need not be a PHC: pair it with a synthetic [`Ioctl`] to run
    /// against a virtual clock, or pass a real `/dev/ptpN` fd obtained
    /// elsewhere. Writability is derived from the fd's `O_ACCMODE`.
    ///
    /// ```
    /// # use std::fs::File;
    /// # use std::os::fd::OwnedFd;
    /// # use std::io;
    /// # use std::os::fd::BorrowedFd;
    /// # use ptp_clock::{Ioctl, IoctlRequest, PtpClock, SystemIoctl};
    /// let fd: OwnedFd = File::open("/dev/null").unwrap().into();
    /// let clock = PtpClock::from_fd(fd, Box::new(SystemIoctl));
    /// assert!(!clock.is_writable());
    /// ```
    pub fn from_fd(fd: OwnedFd, ioctl: Box<dyn Ioctl + Send + Sync>) -> Self {
        let writable = fd_is_writable(fd.as_raw_fd());
        Self {
            fd,
            ioctl,
            writable,
        }
    }

    /// Borrows the underlying file descriptor.
    ///
    /// The escape hatch for the event queue: `poll(2)`, `read(2)`, or any other
    /// file operation the crate does not wrap.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Whether the fd was opened for writing.
    ///
    /// A read-only clock can report capabilities, read the time, and sample
    /// system offsets; steering it fails with
    /// [`ErrorKind::PermissionDenied`](crate::ErrorKind::PermissionDenied).
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Reads the clock capabilities.
    ///
    /// `PTP_CLOCK_GETCAPS2` is tried first and `PTP_CLOCK_GETCAPS` is used when
    /// the kernel reports `ENOTTY`, so the same call works on kernels older
    /// than 3.12.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// let caps = ptp_clock::PtpClock::open(0)?.capabilities()?;
    /// if caps.has_ext_ts(0) {
    ///     // channel 0 can be configured
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn capabilities(&self) -> Result<Capabilities, Error> {
        let mut raw: RawCaps = zeroed();
        self.dispatch_v2(
            Operation::Capabilities,
            IoctlRequest::CLOCK_GETCAPS2,
            IoctlRequest::CLOCK_GETCAPS,
            as_bytes_mut(&mut raw),
        )?;
        Ok(Capabilities::from_raw(&raw))
    }

    /// Reads the current PHC time via `clock_gettime(2)` on the fd.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// let now = clock.get_time()?;
    /// assert!(now.nanos() < 1_000_000_000);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_time(&self) -> Result<ClockTime, Error> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: ts is a valid writable timespec.
        let ret = unsafe { libc::clock_gettime(self.clockid(), &mut ts) };
        if ret < 0 {
            return Err(Error::syscall(
                Operation::GetTime,
                io::Error::last_os_error(),
            ));
        }
        Ok(ClockTime::from_timespec(ts))
    }

    /// Sets the PHC time via `clock_settime(2)` on the fd.
    ///
    /// Requires a writable fd. The kernel reports `EBUSY` while the clock is
    /// steered by a virtual clock, and `EINVAL` for a timestamp that is not a
    /// valid wall-clock value.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// use ptp_clock::ClockTime;
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// clock.set_time(ClockTime::new(1_700_000_000, 0))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_time(&self, time: ClockTime) -> Result<(), Error> {
        let ts = time
            .to_timespec()
            .ok_or_else(|| Error::invalid(Operation::SetTime, "timestamp does not fit time_t"))?;
        // SAFETY: ts points to a valid timespec.
        let ret = unsafe { libc::clock_settime(self.clockid(), &ts) };
        self.check(Operation::SetTime, ret)
    }

    /// Adjusts the clock frequency by `ppb` parts per billion.
    ///
    /// The kernel expresses `ADJ_FREQUENCY` in scaled ppm (a 16-bit binary
    /// fraction); the conversion here is the exact inverse, so the frequency
    /// the driver sees is the requested one. Values beyond
    /// [`Capabilities::max_adj`] are rejected by the kernel with `ERANGE`
    /// ([`ErrorKind::InvalidArgument`](crate::ErrorKind)).
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// clock.adjust_freq(-250)?;
    /// assert_eq!(clock.frequency_ppb()?, -250);
    /// # Ok(())
    /// # }
    /// ```
    pub fn adjust_freq(&self, ppb: i64) -> Result<(), Error> {
        let scaled = ppb_to_scaled_ppm(ppb).ok_or_else(|| {
            Error::invalid(
                Operation::AdjustFrequency,
                "frequency does not fit the scaled ppm field",
            )
        })?;
        let mut tx = zeroed_timex();
        tx.modes = libc::ADJ_FREQUENCY;
        tx.freq = scaled;
        self.adjtime(Operation::AdjustFrequency, &mut tx)
    }

    /// Reads back the frequency most recently set with [`Self::adjust_freq`].
    ///
    /// `clock_adjtime(2)` with `modes = 0` returns the dialed frequency without
    /// changing anything, so this works on a read-only fd.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// let ppb = ptp_clock::PtpClock::open(0)?.frequency_ppb()?;
    /// println!("dialed: {ppb} ppb");
    /// # Ok(())
    /// # }
    /// ```
    pub fn frequency_ppb(&self) -> Result<i64, Error> {
        let mut tx = zeroed_timex();
        tx.modes = 0;
        self.adjtime(Operation::ReadFrequency, &mut tx)?;
        Ok(scaled_ppm_to_ppb(widen(tx.freq)))
    }

    /// Adjusts the clock phase by `nanos` nanoseconds.
    ///
    /// Only available when [`Capabilities::adjust_phase`] is set; the kernel
    /// otherwise reports `EOPNOTSUPP`. Offsets beyond
    /// [`Capabilities::max_phase_adj`] are rejected with `ERANGE`.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// if clock.capabilities()?.has_adjust_phase() {
    ///     clock.adjust_phase(120)?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn adjust_phase(&self, nanos: i64) -> Result<(), Error> {
        let offset = c_long::try_from(nanos).map_err(|_| {
            Error::invalid(Operation::AdjustPhase, "phase offset does not fit a long")
        })?;
        let mut tx = zeroed_timex();
        tx.modes = libc::ADJ_OFFSET | libc::ADJ_NANO;
        tx.offset = offset;
        self.adjtime(Operation::AdjustPhase, &mut tx)
    }

    /// Steps the clock by `offset` via `ADJ_SETOFFSET`.
    ///
    /// This is a discontinuous correction: the clock jumps. Use
    /// [`Self::adjust_freq`] or [`Self::adjust_phase`] for continuous
    /// corrections.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// use ptp_clock::ClockTime;
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// clock.step_time(ClockTime::from_nanos(-500))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn step_time(&self, offset: ClockTime) -> Result<(), Error> {
        let mut tx = zeroed_timex();
        tx.modes = libc::ADJ_SETOFFSET | libc::ADJ_NANO;
        tx.time.tv_sec = libc::time_t::try_from(offset.secs())
            .map_err(|_| Error::invalid(Operation::StepTime, "offset does not fit time_t"))?;
        tx.time.tv_usec = offset.nanos() as c_long;
        self.adjtime(Operation::StepTime, &mut tx)
    }

    fn adjtime(&self, operation: Operation, tx: &mut libc::timex) -> Result<(), Error> {
        // SAFETY: tx is a valid, initialized timex.
        let ret = unsafe { libc::clock_adjtime(self.clockid(), tx) };
        // clock_adjtime returns the clock state on success, so only negatives
        // are errors.
        self.check(operation, ret)
    }

    fn check(&self, operation: Operation, ret: std::ffi::c_int) -> Result<(), Error> {
        if ret < 0 {
            Err(Error::syscall(operation, io::Error::last_os_error()))
        } else {
            Ok(())
        }
    }

    fn clockid(&self) -> libc::clockid_t {
        fd_to_clockid(self.fd.as_raw_fd())
    }

    pub(crate) fn dispatch(
        &self,
        operation: Operation,
        request: IoctlRequest,
        buffer: &mut [u8],
    ) -> Result<(), Error> {
        self.ioctl
            .ioctl(self.fd.as_fd(), request, buffer)
            .map_err(|err| Error::ioctl(operation, request, err))
    }

    /// Dispatches a V2 request, falling back to V1 when the kernel does not
    /// know the V2 number (`ENOTTY`).
    pub(crate) fn dispatch_v2(
        &self,
        operation: Operation,
        v2: IoctlRequest,
        v1: IoctlRequest,
        buffer: &mut [u8],
    ) -> Result<(), Error> {
        match self.dispatch(operation, v2, buffer) {
            Err(Error::Ioctl { source, .. }) if source.raw_os_error() == Some(libc::ENOTTY) => {
                self.dispatch(operation, v1, buffer)
            }
            result => result,
        }
    }

    /// Scalar-argument variant of [`Self::dispatch_v2`].
    pub(crate) fn dispatch_value_v2(
        &self,
        operation: Operation,
        v2: IoctlRequest,
        v1: IoctlRequest,
        value: c_long,
    ) -> Result<(), Error> {
        let dispatch = |request| {
            self.ioctl
                .ioctl_value(self.fd.as_fd(), request, value as libc::c_ulong)
                .map_err(|err| Error::ioctl(operation, request, err))
        };
        match dispatch(v2) {
            Err(Error::Ioctl { source, .. }) if source.raw_os_error() == Some(libc::ENOTTY) => {
                dispatch(v1)
            }
            result => result,
        }
    }
}

fn zeroed_timex() -> libc::timex {
    // SAFETY: timex is plain data; all-zero is a valid value.
    unsafe { std::mem::zeroed() }
}

fn fd_is_writable(fd: RawFd) -> bool {
    // SAFETY: F_GETFL does not modify the descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    flags >= 0 && (flags & libc::O_ACCMODE) != libc::O_RDONLY
}

/// Encodes a file descriptor as a dynamic POSIX clock id (`CLOCKFD`).
fn fd_to_clockid(fd: RawFd) -> libc::clockid_t {
    ((!(fd as u32)) << 3 | 3) as libc::clockid_t
}

/// The inverse of [`fd_to_clockid`], used by the tests.
#[cfg(test)]
fn clockid_to_fd(clockid: libc::clockid_t) -> u32 {
    (!(clockid >> 3)) as u32
}

/// Converts parts per billion into the kernel's scaled ppm field.
///
/// The kernel computes `ppb = ((1 + ppm) * 125) >> 13`, so `ppm` must be
/// `ceil(ppb * 8192 / 125) - 1` for the round trip to be exact.
fn ppb_to_scaled_ppm(ppb: i64) -> Option<c_long> {
    let scaled = i128::from(ppb) * 8192;
    let ceil = scaled.div_euclid(125) + i128::from(scaled.rem_euclid(125) != 0);
    c_long::try_from(ceil - 1).ok()
}

/// Widens a platform integer to `i64`; a no-op where `c_long` is already 64-bit.
fn widen(value: impl Into<i64>) -> i64 {
    value.into()
}

/// Applies the kernel's `scaled_ppm_to_ppb` conversion.
fn scaled_ppm_to_ppb(ppm: i64) -> i64 {
    ((i128::from(1 + ppm) * 125) >> 13) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clockid_round_trips_through_the_fd() {
        for fd in [0u32, 1, 2, 3, 7, 1023, (1 << 20) - 1] {
            let clockid = fd_to_clockid(fd as RawFd);
            assert_eq!(clockid_to_fd(clockid), fd);
        }
        // CLOCKFD is 3, and the encoding inverts the fd.
        assert_eq!(fd_to_clockid(0), !0 << 3 | 3);
    }

    #[test]
    fn writability_follows_the_open_mode() {
        use std::fs::OpenOptions;
        use std::io::Write;

        let path = std::env::temp_dir().join(format!("ptp-clock-fd-{}", std::process::id()));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"")
            .unwrap();

        let read_only = std::fs::File::open(&path).unwrap();
        assert!(!fd_is_writable(read_only.as_raw_fd()));

        let read_write = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(fd_is_writable(read_write.as_raw_fd()));

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn clock_time_normalizes_and_formats() {
        assert_eq!(
            ClockTime::new(-1, 1_500_000_000),
            ClockTime::new(0, 500_000_000)
        );
        assert_eq!(
            ClockTime::from_nanos(i64::MIN).as_nanos(),
            i128::from(i64::MIN)
        );
        assert_eq!(ClockTime::new(0, 0).to_string(), "0.000000000s");
        assert_eq!(ClockTime::new(-1, 500_000_000).to_string(), "-0.500000000s");
    }

    #[test]
    fn ppb_conversion_is_exact_across_the_range() {
        for ppb in [
            -1_000_000_000i64,
            -100_000_000,
            -1000,
            -1,
            0,
            1,
            999,
            1000,
            100_000_000,
        ] {
            let scaled = ppb_to_scaled_ppm(ppb).unwrap();
            assert_eq!(scaled_ppm_to_ppb(widen(scaled)), ppb, "ppb={ppb}");
        }
        // Every single ppb in a practical range round-trips.
        for ppb in -70_000i64..=70_000 {
            let scaled = ppb_to_scaled_ppm(ppb).unwrap();
            assert_eq!(scaled_ppm_to_ppb(widen(scaled)), ppb);
        }
    }

    #[test]
    fn kernel_scaled_ppm_examples() {
        // ((1 + ppm) * 125) >> 13 mirrors include/linux/ptp_clock_kernel.h.
        assert_eq!(scaled_ppm_to_ppb(-1), 0);
        assert_eq!(scaled_ppm_to_ppb(0), 0);
        assert_eq!(scaled_ppm_to_ppb(65535), 1000);
        assert_eq!(scaled_ppm_to_ppb(-65), -1);
    }
}
