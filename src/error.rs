//! Typed errors: what failed, how the kernel classified it, and why.

use crate::ioctl::IoctlRequest;
use std::fmt;
use std::io;
use std::path::PathBuf;

/// How the kernel classified a failure.
///
/// This collapses the errno a syscall reported into the handful of cases a
/// caller can act on, while the original error stays available through
/// [`Error::io_error`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The kernel or driver does not support this (`ENOTTY`, `EOPNOTSUPP`).
    ///
    /// For V2 requests this is also what an older kernel reports before the
    /// crate falls back to the V1 ioctl.
    NotSupported,
    /// The kernel rejected an argument (`EINVAL`, `ERANGE`, `EOVERFLOW`).
    InvalidArgument,
    /// The resource is in use or the clock is otherwise unavailable (`EBUSY`).
    ///
    /// A PHC steered by a virtual clock is "free running" and reports `EBUSY`
    /// for `set_time` and frequency adjustments.
    Busy,
    /// The process lacks permission (`EPERM`, `EACCES`).
    ///
    /// The fd must be opened read-write for any operation that changes the
    /// clock, and PPS enablement additionally requires `CAP_SYS_TIME`.
    PermissionDenied,
    /// The device or network interface does not exist (`ENOENT`, `ENODEV`).
    NotFound,
    /// Any other OS error.
    Other,
}

impl ErrorKind {
    fn from_errno(errno: i32) -> Self {
        match errno {
            libc::ENOTTY | libc::EOPNOTSUPP | libc::ENOSYS => Self::NotSupported,
            libc::EINVAL | libc::ERANGE | libc::EOVERFLOW => Self::InvalidArgument,
            libc::EBUSY => Self::Busy,
            libc::EPERM | libc::EACCES => Self::PermissionDenied,
            libc::ENOENT | libc::ENODEV | libc::ENXIO => Self::NotFound,
            _ => Self::Other,
        }
    }
}

/// The operation that failed.
///
/// The names follow the public API, so an error is readable without the
/// backtrace of where it was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Operation {
    /// Opening the device node.
    Open,
    /// Reading clock capabilities.
    Capabilities,
    /// Reading the hardware clock.
    GetTime,
    /// Setting the hardware clock.
    SetTime,
    /// Adjusting the clock frequency.
    AdjustFrequency,
    /// Reading the dialed frequency.
    ReadFrequency,
    /// Adjusting the clock phase.
    AdjustPhase,
    /// Stepping the clock.
    StepTime,
    /// Sampling system/PHC offsets.
    SysOffset,
    /// Cross-timestamping the PHC with the system clock.
    SysOffsetPrecise,
    /// Sampling pre/PHC/post clock triples.
    SysOffsetExtended,
    /// Configuring an external timestamp channel.
    ExtTs,
    /// Configuring a periodic output.
    PerOut,
    /// Reading a pin function.
    PinGetFunction,
    /// Assigning a pin function.
    PinSetFunction,
    /// Enabling or disabling the PPS signal.
    EnablePps,
    /// Reading the hardware timestamping configuration.
    HwTimestampGet,
    /// Applying a hardware timestamping configuration.
    HwTimestampSet,
    /// Reading timestamp events from the event queue.
    ReadEvents,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Open => "open",
            Self::Capabilities => "capabilities",
            Self::GetTime => "get_time",
            Self::SetTime => "set_time",
            Self::AdjustFrequency => "adjust_freq",
            Self::ReadFrequency => "frequency_ppb",
            Self::AdjustPhase => "adjust_phase",
            Self::StepTime => "step_time",
            Self::SysOffset => "sys_offset",
            Self::SysOffsetPrecise => "sys_offset_precise",
            Self::SysOffsetExtended => "sys_offset_extended",
            Self::ExtTs => "request_ext_ts",
            Self::PerOut => "request_perout",
            Self::PinGetFunction => "pin_function",
            Self::PinSetFunction => "set_pin_function",
            Self::EnablePps => "enable_pps",
            Self::HwTimestampGet => "HwTimestamping::get",
            Self::HwTimestampSet => "HwTimestamping::set",
            Self::ReadEvents => "read_events",
        };
        f.write_str(name)
    }
}

/// An error from a PHC or hardware timestamping operation.
///
/// Every variant names the failed operation and, when the failure came from the
/// kernel, keeps the original [`io::Error`] as its source. [`Error::kind`]
/// classifies the errno into an actionable [`ErrorKind`].
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A device node could not be opened.
    Open {
        /// The path that was opened.
        path: PathBuf,
        /// The underlying OS error.
        source: io::Error,
    },
    /// An ioctl request failed.
    Ioctl {
        /// The operation being performed.
        operation: Operation,
        /// The request number that was dispatched.
        request: IoctlRequest,
        /// The errno reported by the kernel.
        source: io::Error,
    },
    /// A non-ioctl syscall or a read from the event queue failed.
    Syscall {
        /// The operation being performed.
        operation: Operation,
        /// The errno reported by the kernel.
        source: io::Error,
    },
    /// A request was rejected before it reached the kernel.
    InvalidRequest {
        /// The operation being performed.
        operation: Operation,
        /// Why the request cannot be expressed.
        reason: &'static str,
    },
}

impl Error {
    pub(crate) fn open(path: &std::path::Path, source: io::Error) -> Self {
        Self::Open {
            path: path.to_path_buf(),
            source,
        }
    }

    pub(crate) fn ioctl(operation: Operation, request: IoctlRequest, source: io::Error) -> Self {
        Self::Ioctl {
            operation,
            request,
            source,
        }
    }

    pub(crate) fn syscall(operation: Operation, source: io::Error) -> Self {
        Self::Syscall { operation, source }
    }

    pub(crate) fn invalid(operation: Operation, reason: &'static str) -> Self {
        Self::InvalidRequest { operation, reason }
    }

    /// The operation that failed.
    pub fn operation(&self) -> Operation {
        match self {
            Self::Open { .. } => Operation::Open,
            Self::Ioctl { operation, .. } | Self::Syscall { operation, .. } => *operation,
            Self::InvalidRequest { operation, .. } => *operation,
        }
    }

    /// How the failure is classified.
    ///
    /// A request rejected before the syscall is always
    /// [`ErrorKind::InvalidArgument`].
    pub fn kind(&self) -> ErrorKind {
        match self.raw_os_error() {
            Some(errno) => ErrorKind::from_errno(errno),
            None => ErrorKind::InvalidArgument,
        }
    }

    /// The raw `errno` value, when the failure came from the kernel.
    pub fn raw_os_error(&self) -> Option<i32> {
        self.io_error().and_then(io::Error::raw_os_error)
    }

    /// The underlying OS error, when the failure came from the kernel.
    pub fn io_error(&self) -> Option<&io::Error> {
        match self {
            Self::Open { source, .. }
            | Self::Ioctl { source, .. }
            | Self::Syscall { source, .. } => Some(source),
            Self::InvalidRequest { .. } => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open { path, source } => write!(f, "cannot open {}: {source}", path.display()),
            Self::Ioctl {
                operation,
                request,
                source,
            } => {
                write!(f, "{operation} failed ({request}): {source}")
            }
            Self::Syscall { operation, source } => write!(f, "{operation} failed: {source}"),
            Self::InvalidRequest { operation, reason } => {
                write!(f, "{operation} rejected the request: {reason}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.io_error()
            .map(|err| err as &(dyn std::error::Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errno_classification() {
        let cases = [
            (libc::ENOTTY, ErrorKind::NotSupported),
            (libc::EOPNOTSUPP, ErrorKind::NotSupported),
            (libc::EINVAL, ErrorKind::InvalidArgument),
            (libc::ERANGE, ErrorKind::InvalidArgument),
            (libc::EBUSY, ErrorKind::Busy),
            (libc::EPERM, ErrorKind::PermissionDenied),
            (libc::EACCES, ErrorKind::PermissionDenied),
            (libc::ENODEV, ErrorKind::NotFound),
            (libc::EIO, ErrorKind::Other),
        ];
        for (errno, kind) in cases {
            assert_eq!(ErrorKind::from_errno(errno), kind);
        }
    }

    #[test]
    fn invalid_request_has_no_source_and_reports_the_operation() {
        let err = Error::invalid(Operation::SysOffset, "n_samples exceeds PTP_MAX_SAMPLES");
        assert_eq!(err.operation(), Operation::SysOffset);
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert!(err.raw_os_error().is_none());
        assert!(std::error::Error::source(&err).is_none());
        assert!(err.to_string().contains("PTP_MAX_SAMPLES"));
    }

    #[test]
    fn ioctl_error_keeps_the_request_and_errno() {
        let err = Error::ioctl(
            Operation::ExtTs,
            IoctlRequest::EXTTS_REQUEST2,
            io::Error::from_raw_os_error(libc::EBUSY),
        );
        assert_eq!(err.kind(), ErrorKind::Busy);
        assert_eq!(err.raw_os_error(), Some(libc::EBUSY));
        assert!(std::error::Error::source(&err).is_some());
        assert!(err.to_string().contains("PTP_EXTTS_REQUEST2"));
    }
}
