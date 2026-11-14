//! The ioctl dispatch boundary and the kernel UAPI structures it moves.
//!
//! Requests are built from, and responses parsed into, plain `#[repr(C)]`
//! structures whose sizes are asserted against the UAPI. All of that work is
//! pure, so an injected [`Ioctl`] implementation can exercise the success and
//! error paths of every operation without a PHC device.

use crate::clock::ClockTime;
use core::ffi::{c_int, c_ulong, c_void};
use core::fmt;
use core::mem::size_of;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

/// One ioctl request number, as understood by the kernel.
///
/// The constants mirror the UAPI headers. Numbers are computed from the
/// platform's `_IOC` layout rather than hardcoded, so they stay correct on
/// architectures with a different direction or size field width.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct IoctlRequest(c_ulong);

impl IoctlRequest {
    /// `PTP_CLOCK_GETCAPS`: read [`Capabilities`](crate::Capabilities).
    pub const CLOCK_GETCAPS: Self = Self::ptp(IOC_READ, 1, size_of::<RawCaps>());
    /// `PTP_EXTTS_REQUEST`: configure an external timestamp channel.
    pub const EXTTS_REQUEST: Self = Self::ptp(IOC_WRITE, 2, size_of::<RawExtTsRequest>());
    /// `PTP_PEROUT_REQUEST`: configure a periodic output.
    pub const PEROUT_REQUEST: Self = Self::ptp(IOC_WRITE, 3, size_of::<RawPerOut>());
    /// `PTP_ENABLE_PPS`: enable or disable the PPS signal.
    pub const ENABLE_PPS: Self = Self::ptp(IOC_WRITE, 4, size_of::<c_int>());
    /// `PTP_SYS_OFFSET`: sample system/PHC clock pairs.
    pub const SYS_OFFSET: Self = Self::ptp(IOC_WRITE, 5, size_of::<RawSysOffset>());
    /// `PTP_PIN_GETFUNC`: read a pin function.
    pub const PIN_GETFUNC: Self = Self::ptp(IOC_READ_WRITE, 6, size_of::<RawPinDesc>());
    /// `PTP_PIN_SETFUNC`: assign a pin function.
    pub const PIN_SETFUNC: Self = Self::ptp(IOC_WRITE, 7, size_of::<RawPinDesc>());
    /// `PTP_SYS_OFFSET_PRECISE`: cross-timestamp the PHC with the system clock.
    pub const SYS_OFFSET_PRECISE: Self =
        Self::ptp(IOC_READ_WRITE, 8, size_of::<RawSysOffsetPrecise>());
    /// `PTP_SYS_OFFSET_EXTENDED`: sample pre/PHC/post clock triples.
    pub const SYS_OFFSET_EXTENDED: Self =
        Self::ptp(IOC_READ_WRITE, 9, size_of::<RawSysOffsetExtended>());

    /// `PTP_CLOCK_GETCAPS2`: V2 variant of [`Self::CLOCK_GETCAPS`].
    pub const CLOCK_GETCAPS2: Self = Self::ptp(IOC_READ, 10, size_of::<RawCaps>());
    /// `PTP_EXTTS_REQUEST2`: V2 variant of [`Self::EXTTS_REQUEST`].
    pub const EXTTS_REQUEST2: Self = Self::ptp(IOC_WRITE, 11, size_of::<RawExtTsRequest>());
    /// `PTP_PEROUT_REQUEST2`: V2 variant of [`Self::PEROUT_REQUEST`].
    pub const PEROUT_REQUEST2: Self = Self::ptp(IOC_WRITE, 12, size_of::<RawPerOut>());
    /// `PTP_ENABLE_PPS2`: V2 variant of [`Self::ENABLE_PPS`].
    pub const ENABLE_PPS2: Self = Self::ptp(IOC_WRITE, 13, size_of::<c_int>());
    /// `PTP_SYS_OFFSET2`: V2 variant of [`Self::SYS_OFFSET`].
    pub const SYS_OFFSET2: Self = Self::ptp(IOC_WRITE, 14, size_of::<RawSysOffset>());
    /// `PTP_PIN_GETFUNC2`: V2 variant of [`Self::PIN_GETFUNC`].
    pub const PIN_GETFUNC2: Self = Self::ptp(IOC_READ_WRITE, 15, size_of::<RawPinDesc>());
    /// `PTP_PIN_SETFUNC2`: V2 variant of [`Self::PIN_SETFUNC`].
    pub const PIN_SETFUNC2: Self = Self::ptp(IOC_READ_WRITE, 16, size_of::<RawPinDesc>());
    /// `PTP_SYS_OFFSET_PRECISE2`: V2 variant of [`Self::SYS_OFFSET_PRECISE`].
    pub const SYS_OFFSET_PRECISE2: Self =
        Self::ptp(IOC_READ_WRITE, 17, size_of::<RawSysOffsetPrecise>());
    /// `PTP_SYS_OFFSET_EXTENDED2`: V2 variant of [`Self::SYS_OFFSET_EXTENDED`].
    pub const SYS_OFFSET_EXTENDED2: Self =
        Self::ptp(IOC_READ_WRITE, 18, size_of::<RawSysOffsetExtended>());

    /// `PTP_MASK_CLEAR_ALL`: clear the per-open event queue mask (Linux 6.14+).
    ///
    /// Not used by this crate; exposed so callers can reach it through
    /// [`Ioctl`] without redefining the number.
    pub const MASK_CLEAR_ALL: Self = Self::ptp(IOC_NONE, 19, 0);
    /// `PTP_MASK_EN_SINGLE`: enable one event channel in this open (Linux 6.14+).
    pub const MASK_EN_SINGLE: Self = Self::ptp(IOC_WRITE, 20, size_of::<c_int>());
    /// `PTP_SYS_OFFSET_PRECISE_CYCLES`: cross-timestamp in cycles (Linux 6.15+).
    pub const SYS_OFFSET_PRECISE_CYCLES: Self =
        Self::ptp(IOC_READ_WRITE, 21, size_of::<RawSysOffsetPrecise>());
    /// `PTP_SYS_OFFSET_EXTENDED_CYCLES`: extended offset in cycles (Linux 6.15+).
    pub const SYS_OFFSET_EXTENDED_CYCLES: Self =
        Self::ptp(IOC_READ_WRITE, 22, size_of::<RawSysOffsetExtended>());

    /// `SIOCSHWTSTAMP`: configure hardware timestamping on a net device.
    pub const HWTSTAMP_SET: Self = Self(0x89b0);
    /// `SIOCGHWTSTAMP`: read the hardware timestamping configuration.
    pub const HWTSTAMP_GET: Self = Self(0x89b1);

    const fn ptp(dir: u32, nr: u8, size: usize) -> Self {
        Self(ioc(dir, b'=', nr, size))
    }

    /// Builds a request from a raw `ioctl(2)` number.
    ///
    /// The escape hatch for ioctls this crate does not name. The number is
    /// passed through unchanged, so the caller owns the argument layout.
    ///
    /// ```
    /// use ptp_clock::IoctlRequest;
    ///
    /// let raw = IoctlRequest::from_raw(0x89b0);
    /// assert_eq!(raw, IoctlRequest::HWTSTAMP_SET);
    /// assert_eq!(IoctlRequest::from_raw(0x1234).name(), None);
    /// ```
    pub const fn from_raw(raw: c_ulong) -> Self {
        Self(raw)
    }

    /// The raw request number passed to `ioctl(2)`.
    pub const fn raw(self) -> c_ulong {
        self.0
    }

    /// The UAPI macro name, when this crate knows it.
    pub const fn name(self) -> Option<&'static str> {
        Some(match self {
            Self::CLOCK_GETCAPS => "PTP_CLOCK_GETCAPS",
            Self::EXTTS_REQUEST => "PTP_EXTTS_REQUEST",
            Self::PEROUT_REQUEST => "PTP_PEROUT_REQUEST",
            Self::ENABLE_PPS => "PTP_ENABLE_PPS",
            Self::SYS_OFFSET => "PTP_SYS_OFFSET",
            Self::PIN_GETFUNC => "PTP_PIN_GETFUNC",
            Self::PIN_SETFUNC => "PTP_PIN_SETFUNC",
            Self::SYS_OFFSET_PRECISE => "PTP_SYS_OFFSET_PRECISE",
            Self::SYS_OFFSET_EXTENDED => "PTP_SYS_OFFSET_EXTENDED",
            Self::CLOCK_GETCAPS2 => "PTP_CLOCK_GETCAPS2",
            Self::EXTTS_REQUEST2 => "PTP_EXTTS_REQUEST2",
            Self::PEROUT_REQUEST2 => "PTP_PEROUT_REQUEST2",
            Self::ENABLE_PPS2 => "PTP_ENABLE_PPS2",
            Self::SYS_OFFSET2 => "PTP_SYS_OFFSET2",
            Self::PIN_GETFUNC2 => "PTP_PIN_GETFUNC2",
            Self::PIN_SETFUNC2 => "PTP_PIN_SETFUNC2",
            Self::SYS_OFFSET_PRECISE2 => "PTP_SYS_OFFSET_PRECISE2",
            Self::SYS_OFFSET_EXTENDED2 => "PTP_SYS_OFFSET_EXTENDED2",
            Self::MASK_CLEAR_ALL => "PTP_MASK_CLEAR_ALL",
            Self::MASK_EN_SINGLE => "PTP_MASK_EN_SINGLE",
            Self::SYS_OFFSET_PRECISE_CYCLES => "PTP_SYS_OFFSET_PRECISE_CYCLES",
            Self::SYS_OFFSET_EXTENDED_CYCLES => "PTP_SYS_OFFSET_EXTENDED_CYCLES",
            Self::HWTSTAMP_SET => "SIOCSHWTSTAMP",
            Self::HWTSTAMP_GET => "SIOCGHWTSTAMP",
            _ => return None,
        })
    }
}

impl fmt::Debug for IoctlRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "ioctl({:#x})", self.0),
        }
    }
}

impl fmt::Display for IoctlRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "ioctl({:#x})", self.0),
        }
    }
}

/// Dispatches ioctls for a file descriptor.
///
/// [`SystemIoctl`] performs the real `ioctl(2)` call. Implement this trait to
/// substitute a synthetic kernel in tests, or to drive a virtual device: the
/// implementation receives the request number and a mutable byte slice holding
/// the argument structure, exactly as the kernel sees it.
///
/// ```
/// use std::io;
/// use std::os::fd::BorrowedFd;
/// use ptp_clock::{Ioctl, IoctlRequest, SystemIoctl};
///
/// #[derive(Debug)]
/// struct Broken;
///
/// impl Ioctl for Broken {
///     fn ioctl(&self, _: BorrowedFd<'_>, _: IoctlRequest, _: &mut [u8]) -> io::Result<()> {
///         Err(io::Error::from_raw_os_error(libc::ENOTTY))
///     }
///
///     fn ioctl_value(&self, _: BorrowedFd<'_>, _: IoctlRequest, _: libc::c_ulong) -> io::Result<()> {
///         Err(io::Error::from_raw_os_error(libc::ENOTTY))
///     }
/// }
///
/// // The real dispatcher has no state; constructing it is free.
/// let _system = SystemIoctl;
/// let _broken: Box<dyn Ioctl + Send + Sync> = Box::new(Broken);
/// ```
pub trait Ioctl {
    /// Perform `request` on `fd`, with `buffer` holding the argument structure.
    ///
    /// The implementation may read the request fields from `buffer` and, for
    /// requests that return data, write the response back into it.
    fn ioctl(&self, fd: BorrowedFd<'_>, request: IoctlRequest, buffer: &mut [u8])
    -> io::Result<()>;

    /// Perform `request` whose argument is a scalar, not a pointer.
    ///
    /// Only `PTP_ENABLE_PPS` and `PTP_MASK_EN_SINGLE` take their argument this
    /// way: the kernel tests the raw value, so a pointer would always be
    /// non-zero.
    fn ioctl_value(
        &self,
        fd: BorrowedFd<'_>,
        request: IoctlRequest,
        value: c_ulong,
    ) -> io::Result<()>;
}

/// The real ioctl dispatcher, backed by `libc::ioctl`.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemIoctl;

impl Ioctl for SystemIoctl {
    fn ioctl(
        &self,
        fd: BorrowedFd<'_>,
        request: IoctlRequest,
        buffer: &mut [u8],
    ) -> io::Result<()> {
        // SAFETY: buffer is a live, writable allocation of at least the size
        // encoded in the request number; the kernel only touches that much.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request.raw(), buffer.as_mut_ptr()) };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn ioctl_value(
        &self,
        fd: BorrowedFd<'_>,
        request: IoctlRequest,
        value: c_ulong,
    ) -> io::Result<()> {
        // SAFETY: the request encodes a scalar argument.
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), request.raw(), value) };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// Number of samples the kernel accepts in one `PTP_SYS_OFFSET*` request.
pub(crate) const MAX_SAMPLES: usize = 25;

// _IOC field geometry (asm-generic, and the variants with a narrower size field).
#[cfg(any(
    target_arch = "mips",
    target_arch = "mips32r6",
    target_arch = "mips64",
    target_arch = "mips64r6",
    target_arch = "powerpc",
    target_arch = "powerpc64",
    target_arch = "sparc",
    target_arch = "sparc64",
))]
const SIZE_BITS: u32 = 13;
#[cfg(not(any(
    target_arch = "mips",
    target_arch = "mips32r6",
    target_arch = "mips64",
    target_arch = "mips64r6",
    target_arch = "powerpc",
    target_arch = "powerpc64",
    target_arch = "sparc",
    target_arch = "sparc64",
)))]
const SIZE_BITS: u32 = 14;

const NR_BITS: u32 = 8;
const TYPE_BITS: u32 = 8;
const TYPE_SHIFT: u32 = NR_BITS;
const SIZE_SHIFT: u32 = TYPE_SHIFT + TYPE_BITS;
const DIR_SHIFT: u32 = SIZE_SHIFT + SIZE_BITS;

const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
const IOC_READ_WRITE: u32 = IOC_READ | IOC_WRITE;

/// Builds an `_IOC` request number, mirroring `include/uapi/asm-generic/ioctl.h`.
const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> c_ulong {
    let size_mask = (1u32 << SIZE_BITS) - 1;
    ((dir << DIR_SHIFT)
        | ((ty as u32) << TYPE_SHIFT)
        | (nr as u32)
        | (((size as u32) & size_mask) << SIZE_SHIFT)) as c_ulong
}

/// A `#[repr(C)]` type that may be reinterpreted as raw bytes.
///
/// # Safety
///
/// Implementors must be plain-old-data: every bit pattern is valid, and the
/// value handed to [`as_bytes_mut`] is fully initialized.
pub(crate) unsafe trait PlainBytes: Sized {}

pub(crate) fn zeroed<T: PlainBytes>() -> T {
    // SAFETY: PlainBytes guarantees every bit pattern is valid.
    unsafe { core::mem::zeroed() }
}

pub(crate) fn as_bytes_mut<T: PlainBytes>(value: &mut T) -> &mut [u8] {
    bytes_mut(core::slice::from_mut(value))
}

pub(crate) fn bytes_mut<T: PlainBytes>(values: &mut [T]) -> &mut [u8] {
    // SAFETY: PlainBytes is POD, so every byte may be written through.
    unsafe {
        core::slice::from_raw_parts_mut(
            values.as_mut_ptr().cast::<u8>(),
            core::mem::size_of_val(values),
        )
    }
}

#[cfg(test)]
pub(crate) fn from_bytes<T: PlainBytes>(bytes: &[u8]) -> T {
    assert!(bytes.len() >= size_of::<T>());
    // SAFETY: length checked; PlainBytes accepts any bit pattern.
    unsafe { bytes.as_ptr().cast::<T>().read_unaligned() }
}

/// `struct ptp_clock_caps`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct RawCaps {
    pub max_adj: c_int,
    pub n_alarm: c_int,
    pub n_ext_ts: c_int,
    pub n_per_out: c_int,
    pub pps: c_int,
    pub n_pins: c_int,
    pub cross_timestamping: c_int,
    pub adjust_phase: c_int,
    pub max_phase_adj: c_int,
    pub rsv: [c_int; 11],
}

unsafe impl PlainBytes for RawCaps {}

/// `struct ptp_extts_request`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RawExtTsRequest {
    pub index: u32,
    pub flags: u32,
    pub rsv: [u32; 2],
}

unsafe impl PlainBytes for RawExtTsRequest {}

/// `struct ptp_perout_request`, with both unions flattened.
///
/// The kernel overlays `start`/`phase` and `on`/`rsv`, so only one meaning of
/// each pair is live, selected by the request flags.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RawPerOut {
    pub start: ClockTime,
    pub period: ClockTime,
    pub index: u32,
    pub flags: u32,
    pub on: ClockTime,
}

unsafe impl PlainBytes for RawPerOut {}

/// `struct ptp_sys_offset`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct RawSysOffset {
    pub n_samples: u32,
    pub rsv: [u32; 3],
    pub ts: [ClockTime; 2 * MAX_SAMPLES + 1],
}

unsafe impl PlainBytes for RawSysOffset {}

/// `struct ptp_sys_offset_precise`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RawSysOffsetPrecise {
    pub device: ClockTime,
    pub sys_realtime: ClockTime,
    pub sys_monoraw: ClockTime,
    pub rsv: [u32; 4],
}

unsafe impl PlainBytes for RawSysOffsetPrecise {}

/// `struct ptp_sys_offset_extended`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct RawSysOffsetExtended {
    pub n_samples: u32,
    pub clockid: c_int,
    pub rsv: [u32; 2],
    pub ts: [[ClockTime; 3]; MAX_SAMPLES],
}

unsafe impl PlainBytes for RawSysOffsetExtended {}

/// `struct ptp_pin_desc`.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct RawPinDesc {
    pub name: [u8; 64],
    pub index: u32,
    pub func: u32,
    pub chan: u32,
    pub rsv: [u32; 5],
}

unsafe impl PlainBytes for RawPinDesc {}

/// `struct ptp_extts_event`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct RawExtTsEvent {
    pub t: ClockTime,
    pub index: u32,
    pub flags: u32,
    pub rsv: [u32; 2],
}

unsafe impl PlainBytes for RawExtTsEvent {}

/// `struct ifreq`, sized to hold the kernel's union on every Linux target.
///
/// Only `ifr_name` and `ifr_data` are addressed; the tail exists because the
/// kernel copies its whole `struct ifreq` (40 bytes on 64-bit) from userspace
/// and would fault on a shorter buffer.
#[repr(C)]
pub(crate) struct IfReq {
    pub name: [u8; libc::IFNAMSIZ],
    pub data: *mut c_void,
    tail: [u8; IFREQ_TAIL],
}

// The kernel's ifreq is 40 bytes on 64-bit and 32 bytes on 32-bit; leave room
// for libc definitions that pad the union a little further.
#[cfg(target_pointer_width = "64")]
const IFREQ_TAIL: usize = 24;
#[cfg(target_pointer_width = "32")]
const IFREQ_TAIL: usize = 20;

impl IfReq {
    pub fn new(ifname: &str, data: *mut c_void) -> Option<Self> {
        if ifname.len() >= libc::IFNAMSIZ {
            return None;
        }
        let mut req: Self = zeroed();
        req.name[..ifname.len()].copy_from_slice(ifname.as_bytes());
        req.data = data;
        Some(req)
    }
}

unsafe impl PlainBytes for IfReq {}

const _: () = {
    assert!(size_of::<RawCaps>() == 80);
    assert!(size_of::<RawExtTsRequest>() == 16);
    assert!(size_of::<RawPerOut>() == 56);
    assert!(size_of::<RawSysOffset>() == 832);
    assert!(size_of::<RawSysOffsetPrecise>() == 64);
    assert!(size_of::<RawSysOffsetExtended>() == 1216);
    assert!(size_of::<RawPinDesc>() == 96);
    assert!(size_of::<RawExtTsEvent>() == 32);
    assert!(size_of::<IfReq>() >= 40);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_the_uapi() {
        // Values cross-checked against <linux/ptp_clock.h> on x86_64.
        let expected: &[(IoctlRequest, c_ulong)] = &[
            (IoctlRequest::CLOCK_GETCAPS, 0x8050_3d01),
            (IoctlRequest::EXTTS_REQUEST, 0x4010_3d02),
            (IoctlRequest::PEROUT_REQUEST, 0x4038_3d03),
            (IoctlRequest::ENABLE_PPS, 0x4004_3d04),
            (IoctlRequest::SYS_OFFSET, 0x4340_3d05),
            (IoctlRequest::PIN_GETFUNC, 0xc060_3d06),
            (IoctlRequest::PIN_SETFUNC, 0x4060_3d07),
            (IoctlRequest::SYS_OFFSET_PRECISE, 0xc040_3d08),
            (IoctlRequest::SYS_OFFSET_EXTENDED, 0xc4c0_3d09),
            (IoctlRequest::CLOCK_GETCAPS2, 0x8050_3d0a),
            (IoctlRequest::EXTTS_REQUEST2, 0x4010_3d0b),
            (IoctlRequest::PEROUT_REQUEST2, 0x4038_3d0c),
            (IoctlRequest::ENABLE_PPS2, 0x4004_3d0d),
            (IoctlRequest::SYS_OFFSET2, 0x4340_3d0e),
            (IoctlRequest::PIN_GETFUNC2, 0xc060_3d0f),
            (IoctlRequest::PIN_SETFUNC2, 0xc060_3d10),
            (IoctlRequest::SYS_OFFSET_PRECISE2, 0xc040_3d11),
            (IoctlRequest::SYS_OFFSET_EXTENDED2, 0xc4c0_3d12),
            (IoctlRequest::MASK_CLEAR_ALL, 0x0000_3d13),
            (IoctlRequest::MASK_EN_SINGLE, 0x4004_3d14),
            (IoctlRequest::SYS_OFFSET_PRECISE_CYCLES, 0xc040_3d15),
            (IoctlRequest::SYS_OFFSET_EXTENDED_CYCLES, 0xc4c0_3d16),
            (IoctlRequest::HWTSTAMP_SET, 0x89b0),
            (IoctlRequest::HWTSTAMP_GET, 0x89b1),
        ];
        for (request, raw) in expected {
            assert_eq!(request.raw(), *raw, "{request:?}");
            assert!(request.name().is_some());
        }
    }

    #[test]
    fn ifreq_places_the_data_pointer_after_the_name() {
        let mut payload = 0i32;
        let req = IfReq::new("eth0", (&raw mut payload).cast()).unwrap();
        assert_eq!(&req.name[..4], b"eth0");
        assert!(core::ptr::eq(req.data.cast::<i32>(), &raw const payload));
        assert!(IfReq::new("0123456789abcdef", core::ptr::null_mut()).is_none());
    }

    #[test]
    fn raw_round_trip_is_byte_exact() {
        let mut raw = ClockTime::new(-3, 14);
        let bytes = as_bytes_mut(&mut raw).to_vec();
        let parsed: ClockTime = from_bytes(&bytes);
        assert_eq!((parsed.secs(), parsed.nanos()), (-3, 14));
        assert_eq!(size_of::<ClockTime>(), 16);
    }
}
