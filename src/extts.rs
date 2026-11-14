//! External timestamp channels: request configuration, RAII handles, and the
//! event queue.

use crate::clock::{ClockTime, PtpClock};
use crate::error::{Error, Operation};
use crate::ioctl::{IoctlRequest, PlainBytes, RawExtTsRequest, as_bytes_mut, bytes_mut};
use std::io;
use std::os::fd::AsRawFd;

/// Flags for [`ExtTsRequest`] and [`ExtTsEvent`].
///
/// The layout is the `ptp_extts_request.flags` bit field. `PTP_STRICT_FLAGS`
/// is never part of a request: the kernel always imposes it for
/// `PTP_EXTTS_REQUEST2`, and the V1 ioctl does not accept it.
///
/// ```
/// use ptp_clock::ExtTsFlags;
///
/// let flags = ExtTsFlags::RISING_EDGE | ExtTsFlags::FALLING_EDGE;
/// assert_eq!(flags, ExtTsFlags::from_bits(0b110));
/// assert!(flags.contains(ExtTsFlags::RISING_EDGE));
/// assert!(!flags.contains(ExtTsFlags::ENABLE_FEATURE));
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ExtTsFlags(u32);

impl ExtTsFlags {
    /// No flags set.
    pub const EMPTY: Self = Self(0);
    /// `PTP_ENABLE_FEATURE`: enable the channel. Added by the crate when a
    /// request is sent; a received event carries it too.
    pub const ENABLE_FEATURE: Self = Self(1 << 0);
    /// `PTP_RISING_EDGE`: timestamp rising edges.
    pub const RISING_EDGE: Self = Self(1 << 1);
    /// `PTP_FALLING_EDGE`: timestamp falling edges.
    pub const FALLING_EDGE: Self = Self(1 << 2);
    /// `PTP_STRICT_FLAGS`: reserved bits must be zero. Implied by the kernel
    /// for V2 requests.
    pub const STRICT_FLAGS: Self = Self(1 << 3);
    /// `PTP_EXT_OFFSET`: request the extra offset timestamps the driver may
    /// provide. Only `PTP_EXTTS_REQUEST2` accepts it.
    pub const EXT_OFFSET: Self = Self(1 << 4);
    /// Both edge flags.
    pub const EDGES: Self = Self(Self::RISING_EDGE.0 | Self::FALLING_EDGE.0);

    /// Builds a flag set from raw bits, keeping unknown bits intact.
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// The raw bit field.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Whether every flag in `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no flag is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Flag set with the edge bits replaced by `edge`.
    fn with_edge(self, edge: ExtTsEdge) -> Self {
        Self((self.0 & !Self::EDGES.0) | edge.bits())
    }
}

impl core::ops::BitOr for ExtTsFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for ExtTsFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl core::ops::BitAnd for ExtTsFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl core::ops::Not for ExtTsFlags {
    type Output = Self;

    fn not(self) -> Self {
        Self(!self.0)
    }
}

/// Which edges of the input signal to timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExtTsEdge {
    /// Timestamp rising edges.
    Rising,
    /// Timestamp falling edges.
    Falling,
    /// Timestamp both edges.
    Both,
}

impl ExtTsEdge {
    const fn bits(self) -> u32 {
        match self {
            Self::Rising => ExtTsFlags::RISING_EDGE.0,
            Self::Falling => ExtTsFlags::FALLING_EDGE.0,
            Self::Both => ExtTsFlags::EDGES.0,
        }
    }
}

/// Configuration for one external timestamp channel.
///
/// ```
/// use ptp_clock::{ExtTsEdge, ExtTsFlags, ExtTsRequest};
///
/// let req = ExtTsRequest::edge(0, ExtTsEdge::Rising);
/// assert_eq!(req.index, 0);
/// assert!(req.flags.contains(ExtTsFlags::RISING_EDGE));
/// assert!(!req.flags.contains(ExtTsFlags::FALLING_EDGE));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExtTsRequest {
    /// Channel index, below [`Capabilities::n_ext_ts`](crate::Capabilities::n_ext_ts).
    pub index: u32,
    /// Flags describing the desired edges and options.
    pub flags: ExtTsFlags,
}

impl ExtTsRequest {
    /// Requests the channel's edges from `edge`.
    pub fn edge(index: u32, edge: ExtTsEdge) -> Self {
        Self {
            index,
            flags: ExtTsFlags::EMPTY.with_edge(edge),
        }
    }

    /// Requests rising edges on `index`.
    pub fn rising(index: u32) -> Self {
        Self::edge(index, ExtTsEdge::Rising)
    }

    /// Requests falling edges on `index`.
    pub fn falling(index: u32) -> Self {
        Self::edge(index, ExtTsEdge::Falling)
    }
}

/// An enabled external timestamp channel.
///
/// The handle disables the channel when dropped, writing the same request with
/// `PTP_ENABLE_FEATURE` cleared. Release errors are ignored in `drop`; call
/// [`release`](Self::release) when the result matters, or
/// [`disarm`](Self::disarm) to leave the channel enabled.
///
/// ```no_run
/// # fn main() -> Result<(), ptp_clock::Error> {
/// use ptp_clock::{ExtTsEdge, ExtTsRequest};
///
/// let clock = ptp_clock::PtpClock::open(0)?;
/// let _handle = clock.request_ext_ts(&ExtTsRequest::edge(0, ExtTsEdge::Both))?;
/// // … the channel is released here …
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct ExtTsHandle<'a> {
    clock: &'a PtpClock,
    request: ExtTsRequest,
    released: bool,
}

impl ExtTsHandle<'_> {
    /// The channel index this handle configured.
    pub fn index(&self) -> u32 {
        self.request.index
    }

    /// The flags this handle configured, without `PTP_ENABLE_FEATURE`.
    pub fn flags(&self) -> ExtTsFlags {
        self.request.flags
    }

    /// Disables the channel, reporting any failure.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// # use ptp_clock::ExtTsRequest;
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// clock.request_ext_ts(&ExtTsRequest::rising(0))?.release()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn release(mut self) -> Result<(), Error> {
        self.released = true;
        self.clock.configure_ext_ts(&self.request, false)
    }

    /// Leaves the channel enabled and gives up the handle.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// # use ptp_clock::ExtTsRequest;
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// clock.request_ext_ts(&ExtTsRequest::rising(0))?.disarm();
    /// # Ok(())
    /// # }
    /// ```
    pub fn disarm(mut self) {
        self.released = true;
    }
}

impl Drop for ExtTsHandle<'_> {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.clock.configure_ext_ts(&self.request, false);
        }
    }
}

/// One hardware timestamp event from the channel's event queue.
///
/// The layout matches `struct ptp_extts_event` (32 bytes), so events are read
/// straight from the fd into a slice of these.
///
/// ```no_run
/// # fn main() -> Result<(), ptp_clock::Error> {
/// use ptp_clock::{ExtTsFlags, ExtTsRequest};
///
/// let clock = ptp_clock::PtpClock::open(0)?;
/// let _handle = clock.request_ext_ts(&ExtTsRequest::rising(0))?;
/// if let Some(event) = clock.read_event()? {
///     assert!(event.flags().contains(ExtTsFlags::RISING_EDGE));
///     println!("channel {} at {}", event.index(), event.time());
/// }
/// # Ok(())
/// # }
/// ```
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ExtTsEvent {
    time: ClockTime,
    index: u32,
    flags: ExtTsFlags,
    reserved: [u32; 2],
}

unsafe impl PlainBytes for ExtTsEvent {}

impl ExtTsEvent {
    /// When the edge occurred, on the PHC.
    pub fn time(&self) -> ClockTime {
        self.time
    }

    /// Which channel produced the event.
    pub fn index(&self) -> u32 {
        self.index
    }

    /// Event flags, as reported by the kernel.
    pub fn flags(&self) -> ExtTsFlags {
        self.flags
    }

    /// The reserved bytes, kept verbatim for future kernel extensions.
    pub fn reserved(&self) -> [u32; 2] {
        self.reserved
    }
}

const _: () = {
    assert!(core::mem::size_of::<ExtTsEvent>() == 32);
};

impl PtpClock {
    /// Enables an external timestamp channel.
    ///
    /// `PTP_EXTTS_REQUEST2` is tried first; kernels without it get
    /// `PTP_EXTTS_REQUEST`, which cannot express
    /// [`ExtTsFlags::EXT_OFFSET`]. The returned handle borrows the clock and
    /// disables the channel on drop.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// use ptp_clock::{ExtTsEdge, ExtTsRequest};
    ///
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// let _handle = clock.request_ext_ts(&ExtTsRequest::edge(0, ExtTsEdge::Rising))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn request_ext_ts(&self, request: &ExtTsRequest) -> Result<ExtTsHandle<'_>, Error> {
        self.configure_ext_ts(request, true)?;
        Ok(ExtTsHandle {
            clock: self,
            request: *request,
            released: false,
        })
    }

    /// Writes the request with `PTP_ENABLE_FEATURE` set or cleared.
    ///
    /// The V2 ioctl is tried first; on `ENOTTY` the V1 ioctl is used only when
    /// the flags fit its mask, because silently dropping `PTP_EXT_OFFSET`
    /// would change the meaning of the request.
    fn configure_ext_ts(&self, request: &ExtTsRequest, enable: bool) -> Result<(), Error> {
        let mut raw = RawExtTsRequest {
            index: request.index,
            flags: request.flags.bits(),
            rsv: [0; 2],
        };
        if enable {
            raw.flags |= ExtTsFlags::ENABLE_FEATURE.bits();
        }
        match self.dispatch(
            Operation::ExtTs,
            IoctlRequest::EXTTS_REQUEST2,
            as_bytes_mut(&mut raw),
        ) {
            Ok(()) => Ok(()),
            Err(Error::Ioctl { source, .. }) if source.raw_os_error() == Some(libc::ENOTTY) => {
                if request.flags.contains(ExtTsFlags::EXT_OFFSET) {
                    return Err(Error::invalid(
                        Operation::ExtTs,
                        "PTP_EXT_OFFSET requires PTP_EXTTS_REQUEST2, which this kernel does not support",
                    ));
                }
                self.dispatch(
                    Operation::ExtTs,
                    IoctlRequest::EXTTS_REQUEST,
                    as_bytes_mut(&mut raw),
                )
            }
            Err(err) => Err(err),
        }
    }

    /// Reads one timestamp event, or `None` when the non-blocking queue is
    /// empty.
    ///
    /// With a blocking fd this waits for the next event. `EAGAIN` is mapped to
    /// `Ok(None)`; the escape hatch [`PtpClock::as_fd`] supports `poll(2)` for
    /// callers that want to wait themselves.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// use ptp_clock::ExtTsRequest;
    ///
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// let _handle = clock.request_ext_ts(&ExtTsRequest::rising(0))?;
    /// match clock.read_event()? {
    ///     Some(event) => println!("{} at {}", event.index(), event.time()),
    ///     None => println!("no event queued"),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn read_event(&self) -> Result<Option<ExtTsEvent>, Error> {
        let mut event = ExtTsEvent::default();
        let buffer = as_bytes_mut(&mut event);
        let read = read_fd(self.as_fd().as_raw_fd(), buffer)?;
        Ok((read == buffer.len()).then_some(event))
    }

    /// Reads as many events as fit, returning how many were stored.
    ///
    /// One `read(2)` may return any number of the 32-byte events up to the
    /// caller's capacity; partial trailing bytes are never produced by the
    /// kernel. `EAGAIN` yields `Ok(0)`.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// use ptp_clock::{ExtTsEvent, ExtTsRequest};
    ///
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// let _handle = clock.request_ext_ts(&ExtTsRequest::rising(0))?;
    /// let mut events = [ExtTsEvent::default(); 8];
    /// let count = clock.read_events(&mut events)?;
    /// for event in &events[..count] {
    ///     println!("{} at {}", event.index(), event.time());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn read_events(&self, events: &mut [ExtTsEvent]) -> Result<usize, Error> {
        if events.is_empty() {
            return Ok(0);
        }
        let buffer = bytes_mut(events);
        let read = read_fd(self.as_fd().as_raw_fd(), buffer)?;
        Ok(read / core::mem::size_of::<ExtTsEvent>())
    }
}

fn read_fd(fd: libc::c_int, buffer: &mut [u8]) -> Result<usize, Error> {
    // SAFETY: buffer is a valid writable allocation of buffer.len() bytes.
    let ret = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if ret < 0 {
        let err = io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(libc::EAGAIN) => Ok(0),
            _ => Err(Error::syscall(Operation::ReadEvents, err)),
        };
    }
    Ok(ret as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_flags_replace_instead_of_accumulating() {
        let req = ExtTsRequest::edge(3, ExtTsEdge::Both);
        assert_eq!(req.flags, ExtTsFlags::EDGES);
        let req = ExtTsRequest {
            index: 1,
            flags: req.flags.with_edge(ExtTsEdge::Falling),
        };
        assert_eq!(req.flags, ExtTsFlags::FALLING_EDGE);
        assert_eq!(req.index, 1);
    }

    #[test]
    fn unknown_flag_bits_survive() {
        let flags = ExtTsFlags::from_bits(0b1000_1101);
        assert_eq!(flags.bits(), 0b1000_1101);
        assert!(flags.contains(ExtTsFlags::ENABLE_FEATURE));
        assert!(flags.contains(ExtTsFlags::STRICT_FLAGS));
        assert!(!flags.contains(ExtTsFlags::EDGES));
    }

    #[test]
    fn event_layout_matches_the_uapi() {
        assert_eq!(core::mem::size_of::<ExtTsEvent>(), 32);
        assert_eq!(
            core::mem::align_of::<ExtTsEvent>(),
            core::mem::align_of::<crate::ioctl::RawExtTsEvent>()
        );
    }
}
