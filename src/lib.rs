//! Complete mechanism layer for the Linux PTP hardware clock (PHC).
//!
//! A PHC is a clock in a network or timekeeping device, exposed as
//! `/dev/ptpN`. The kernel offers it through the `PTP_*` ioctl family plus a
//! POSIX dynamic-clock fd for reading and steering time, and through
//! `SIOC[GS]HWTSTAMP` on the network side. This crate wraps that whole surface
//! with typed structures, capability gating, RAII handles, and errors that name
//! the failed operation:
//!
//! - [`PtpClock`]: open a device, read capabilities, read/set/steer time.
//! - [`PtpClock::sys_offset`] and friends: sample PHC/system offsets.
//! - [`PtpClock::request_ext_ts`]: timestamp external edges; read events from
//!   the fd.
//! - [`PtpClock::request_perout`]: emit periodic signals.
//! - [`PtpClock::pin_function`] / [`PtpClock::set_pin_function`]: pins.
//! - [`PtpClock::enable_pps`]: the PPS signal.
//! - [`HwTimestamping`]: per-interface hardware timestamping configuration.
//!
//! Policy is left to the caller: no event loop, no clock discovery, no servo
//! algorithm. [`PtpClock::as_fd`] and [`PtpClock::open_path`] are the escape
//! hatches.
//!
//! # Example
//!
//! ```no_run
//! use ptp_clock::PtpClock;
//!
//! # fn main() -> Result<(), ptp_clock::Error> {
//! let clock = PtpClock::open(0)?;
//! let caps = clock.capabilities()?;
//! println!("{} ppb, {} external timestamps", caps.max_adj, caps.n_ext_ts);
//! println!("PHC time: {}", clock.get_time()?);
//! # Ok(())
//! # }
//! ```
//!
//! Every ioctl goes through the [`Ioctl`] boundary, so the same code can run
//! against a synthetic kernel:
//!
//! ```
//! use std::fs::File;
//! use std::io;
//! use std::os::fd::{BorrowedFd, OwnedFd};
//!
//! use ptp_clock::{ErrorKind, Ioctl, IoctlRequest, PtpClock};
//!
//! /// A kernel that knows no PTP ioctl at all.
//! struct NoPtp;
//!
//! impl Ioctl for NoPtp {
//!     fn ioctl(&self, _: BorrowedFd<'_>, _: IoctlRequest, _: &mut [u8]) -> io::Result<()> {
//!         Err(io::Error::from_raw_os_error(libc::ENOTTY))
//!     }
//!
//!     fn ioctl_value(&self, _: BorrowedFd<'_>, _: IoctlRequest, _: libc::c_ulong) -> io::Result<()> {
//!         Err(io::Error::from_raw_os_error(libc::ENOTTY))
//!     }
//! }
//!
//! let fd: OwnedFd = File::open("/dev/null").unwrap().into();
//! let clock = PtpClock::from_fd(fd, Box::new(NoPtp));
//! let err = clock.capabilities().unwrap_err();
//! assert_eq!(err.kind(), ErrorKind::NotSupported);
//! ```

#![warn(missing_docs)]

#[cfg(not(target_os = "linux"))]
compile_error!("ptp-clock only supports Linux");

mod clock;
mod error;
mod extts;
mod ioctl;
mod offset;
mod perout;
mod pin;
mod pps;
mod socktstamp;

pub use clock::{Capabilities, ClockTime, PtpClock};
pub use error::{Error, ErrorKind, Operation};
pub use extts::{ExtTsEdge, ExtTsEvent, ExtTsFlags, ExtTsHandle, ExtTsRequest};
pub use ioctl::{Ioctl, IoctlRequest, SystemIoctl};
pub use offset::{
    SysOffset, SysOffsetExtended, SysOffsetExtendedSample, SysOffsetPrecise, SysOffsetSample,
};
pub use perout::{PeroutHandle, PeroutRequest, PeroutStart};
pub use pin::{PinDescription, PinFunc, PinFunction};
pub use socktstamp::{HwRxFilter, HwTimestampConfig, HwTimestamping, HwTxType};
