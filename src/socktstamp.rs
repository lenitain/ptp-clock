//! Network-device hardware timestamping configuration (`SIOC[GS]HWTSTAMP`).

use crate::error::{Error, Operation};
use crate::ioctl::{IfReq, Ioctl, IoctlRequest, PlainBytes, SystemIoctl, as_bytes_mut, zeroed};
use std::os::fd::{AsFd, BorrowedFd};

/// Outgoing timestamping mode (`HWTSTAMP_TX_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HwTxType {
    /// No outgoing packet is hardware timestamped.
    Off,
    /// Hardware timestamp outgoing packets selected with `SOF_TIMESTAMPING_TX_*`.
    On,
    /// Insert the timestamp into Sync packets instead of reporting it.
    OneStepSync,
    /// Insert the timestamp into Sync and PDelay_Resp packets.
    OneStepP2P,
    /// A mode this crate does not know, preserved verbatim.
    Unknown(i32),
}

impl HwTxType {
    fn from_raw(raw: i32) -> Self {
        match raw {
            0 => Self::Off,
            1 => Self::On,
            2 => Self::OneStepSync,
            3 => Self::OneStepP2P,
            other => Self::Unknown(other),
        }
    }

    fn to_raw(self) -> i32 {
        match self {
            Self::Off => 0,
            Self::On => 1,
            Self::OneStepSync => 2,
            Self::OneStepP2P => 3,
            Self::Unknown(raw) => raw,
        }
    }
}

/// Incoming packet filter (`HWTSTAMP_FILTER_*`).
///
/// The driver may replace the requested filter with a more general one; the
/// value returned by [`HwTimestamping::set`] is the filter actually in effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HwRxFilter {
    /// Timestamp no incoming packet.
    None,
    /// Timestamp every incoming packet.
    All,
    /// Report value: the driver timestamps the requested packets and some
    /// others.
    Some,
    /// PTP v1 over UDP, any event packet.
    PtpV1L4Event,
    /// PTP v1 over UDP, Sync.
    PtpV1L4Sync,
    /// PTP v1 over UDP, Delay_Req.
    PtpV1L4DelayReq,
    /// PTP v2 over UDP, any event packet.
    PtpV2L4Event,
    /// PTP v2 over UDP, Sync.
    PtpV2L4Sync,
    /// PTP v2 over UDP, Delay_Req.
    PtpV2L4DelayReq,
    /// PTP v2 over Ethernet, any event packet.
    PtpV2L2Event,
    /// PTP v2 over Ethernet, Sync.
    PtpV2L2Sync,
    /// PTP v2 over Ethernet, Delay_Req.
    PtpV2L2DelayReq,
    /// PTP v2 over any layer, any event packet.
    PtpV2Event,
    /// PTP v2 over any layer, Sync.
    PtpV2Sync,
    /// PTP v2 over any layer, Delay_Req.
    PtpV2DelayReq,
    /// NTP, UDP, all versions.
    NtpAll,
    /// A filter this crate does not know, preserved verbatim.
    Unknown(i32),
}

impl HwRxFilter {
    fn from_raw(raw: i32) -> Self {
        match raw {
            0 => Self::None,
            1 => Self::All,
            2 => Self::Some,
            3 => Self::PtpV1L4Event,
            4 => Self::PtpV1L4Sync,
            5 => Self::PtpV1L4DelayReq,
            6 => Self::PtpV2L4Event,
            7 => Self::PtpV2L4Sync,
            8 => Self::PtpV2L4DelayReq,
            9 => Self::PtpV2L2Event,
            10 => Self::PtpV2L2Sync,
            11 => Self::PtpV2L2DelayReq,
            12 => Self::PtpV2Event,
            13 => Self::PtpV2Sync,
            14 => Self::PtpV2DelayReq,
            15 => Self::NtpAll,
            other => Self::Unknown(other),
        }
    }

    fn to_raw(self) -> i32 {
        match self {
            Self::None => 0,
            Self::All => 1,
            Self::Some => 2,
            Self::PtpV1L4Event => 3,
            Self::PtpV1L4Sync => 4,
            Self::PtpV1L4DelayReq => 5,
            Self::PtpV2L4Event => 6,
            Self::PtpV2L4Sync => 7,
            Self::PtpV2L4DelayReq => 8,
            Self::PtpV2L2Event => 9,
            Self::PtpV2L2Sync => 10,
            Self::PtpV2L2DelayReq => 11,
            Self::PtpV2Event => 12,
            Self::PtpV2Sync => 13,
            Self::PtpV2DelayReq => 14,
            Self::NtpAll => 15,
            Self::Unknown(raw) => raw,
        }
    }
}

/// The hardware timestamping configuration of a network device.
///
/// ```
/// use ptp_clock::{HwRxFilter, HwTimestampConfig, HwTxType};
///
/// let config = HwTimestampConfig {
///     flags: 0,
///     tx_type: HwTxType::On,
///     rx_filter: HwRxFilter::PtpV2L4Event,
/// };
/// assert!(!config.uses_bonded_phc_index());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HwTimestampConfig {
    /// Raw `hwtstamp_config.flags` (`HWTSTAMP_FLAG_*`).
    pub flags: u32,
    /// Outgoing timestamping mode in effect.
    pub tx_type: HwTxType,
    /// Incoming filter in effect.
    pub rx_filter: HwRxFilter,
}

impl HwTimestampConfig {
    /// Whether `HWTSTAMP_FLAG_BONDED_PHC_INDEX` is set, meaning the PHC index
    /// belongs to the bond's active interface and can move on failover.
    pub fn uses_bonded_phc_index(&self) -> bool {
        self.flags & 1 != 0
    }
}

/// Hardware timestamping configuration for a network device.
///
/// ```
/// use ptp_clock::{HwRxFilter, HwTimestamping, HwTxType};
///
/// let timestamping = HwTimestamping::new();
/// // `set` needs CAP_NET_ADMIN and a real device, shown as `no_run` on the method.
/// let _ = (timestamping, HwTxType::Off, HwRxFilter::None);
/// ```
pub struct HwTimestamping {
    ioctl: Box<dyn Ioctl + Send + Sync>,
}

impl std::fmt::Debug for HwTimestamping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HwTimestamping").finish_non_exhaustive()
    }
}

impl HwTimestamping {
    /// Configures timestamping through the real kernel.
    pub fn new() -> Self {
        Self {
            ioctl: Box::new(SystemIoctl),
        }
    }

    /// Configures timestamping through a custom dispatcher.
    ///
    /// Useful for virtual devices and for tests that need to observe what the
    /// kernel would report back.
    pub fn with_ioctl(ioctl: Box<dyn Ioctl + Send + Sync>) -> Self {
        Self { ioctl }
    }

    /// Applies `tx`/`rx` to `ifname` and returns the filter the driver actually
    /// enabled.
    ///
    /// `SIOCSHWTSTAMP` writes the effective configuration back into the
    /// request, and drivers may substitute a more general receive filter; that
    /// effective value is returned rather than the requested one. Requires
    /// `CAP_NET_ADMIN`.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// use ptp_clock::{HwRxFilter, HwTimestamping, HwTxType};
    /// use std::net::UdpSocket;
    ///
    /// let socket = UdpSocket::bind("0.0.0.0:319").expect("bind");
    /// let effective = HwTimestamping::new().set(
    ///     &socket,
    ///     "eth0",
    ///     HwTxType::On,
    ///     HwRxFilter::PtpV2L4Event,
    /// )?;
    /// println!("driver enabled {effective:?}");
    /// # Ok(())
    /// # }
    /// ```
    pub fn set(
        &self,
        socket: impl AsFd,
        ifname: &str,
        tx: HwTxType,
        rx: HwRxFilter,
    ) -> Result<HwRxFilter, Error> {
        let mut config = RawHwTimestamp {
            flags: 0,
            tx_type: tx.to_raw(),
            rx_filter: rx.to_raw(),
        };
        self.dispatch(
            socket.as_fd(),
            Operation::HwTimestampSet,
            IoctlRequest::HWTSTAMP_SET,
            ifname,
            &mut config,
        )?;
        Ok(HwRxFilter::from_raw(config.rx_filter))
    }

    /// Reads the current configuration of `ifname` with `SIOCGHWTSTAMP`.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// use std::net::UdpSocket;
    /// use ptp_clock::HwTimestamping;
    ///
    /// let socket = UdpSocket::bind("0.0.0.0:319").expect("bind");
    /// let config = HwTimestamping::new().get(&socket, "eth0")?;
    /// println!("{:?}", config.rx_filter);
    /// # Ok(())
    /// # }
    /// ```
    pub fn get(&self, socket: impl AsFd, ifname: &str) -> Result<HwTimestampConfig, Error> {
        let mut config: RawHwTimestamp = zeroed();
        self.dispatch(
            socket.as_fd(),
            Operation::HwTimestampGet,
            IoctlRequest::HWTSTAMP_GET,
            ifname,
            &mut config,
        )?;
        Ok(HwTimestampConfig {
            flags: config.flags as u32,
            tx_type: HwTxType::from_raw(config.tx_type),
            rx_filter: HwRxFilter::from_raw(config.rx_filter),
        })
    }

    fn dispatch(
        &self,
        socket: BorrowedFd<'_>,
        operation: Operation,
        request: IoctlRequest,
        ifname: &str,
        config: &mut RawHwTimestamp,
    ) -> Result<(), Error> {
        if ifname.is_empty() {
            return Err(Error::invalid(operation, "interface name is empty"));
        }
        let mut ifreq = IfReq::new(ifname, (config as *mut RawHwTimestamp).cast())
            .ok_or_else(|| Error::invalid(operation, "interface name is longer than IFNAMSIZ"))?;
        self.ioctl
            .ioctl(socket, request, as_bytes_mut(&mut ifreq))
            .map_err(|err| Error::ioctl(operation, request, err))
    }
}

impl Default for HwTimestamping {
    fn default() -> Self {
        Self::new()
    }
}

/// `struct hwtstamp_config`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct RawHwTimestamp {
    flags: i32,
    tx_type: i32,
    rx_filter: i32,
}

unsafe impl PlainBytes for RawHwTimestamp {}

const _: () = assert!(core::mem::size_of::<RawHwTimestamp>() == 12);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_mode_round_trips() {
        for tx in [
            HwTxType::Off,
            HwTxType::On,
            HwTxType::OneStepSync,
            HwTxType::OneStepP2P,
            HwTxType::Unknown(7),
        ] {
            assert_eq!(HwTxType::from_raw(tx.to_raw()), tx);
        }
        for raw in 0..=15 {
            let filter = HwRxFilter::from_raw(raw);
            assert_eq!(filter.to_raw(), raw);
        }
        assert_eq!(HwRxFilter::from_raw(99), HwRxFilter::Unknown(99));
    }

    #[test]
    fn bonded_phc_flag_is_reported() {
        let config = HwTimestampConfig {
            flags: 1,
            tx_type: HwTxType::Off,
            rx_filter: HwRxFilter::None,
        };
        assert!(config.uses_bonded_phc_index());
        let config = HwTimestampConfig { flags: 0, ..config };
        assert!(!config.uses_bonded_phc_index());
    }
}
