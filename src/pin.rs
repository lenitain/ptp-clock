//! Pin function discovery and assignment.

use crate::clock::PtpClock;
use crate::error::{Error, Operation};
use crate::ioctl::{IoctlRequest, RawPinDesc, as_bytes_mut, zeroed};

/// The function a pin performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PinFunc {
    /// The pin is unused.
    None,
    /// The pin drives an external timestamp channel.
    ExtTs,
    /// The pin emits a periodic output.
    Perout,
    /// The pin carries the physical synchronization signal.
    Physync,
    /// A function this crate does not know, preserved verbatim.
    Unknown(u32),
}

impl PinFunc {
    fn from_raw(func: u32) -> Self {
        match func {
            0 => Self::None,
            1 => Self::ExtTs,
            2 => Self::Perout,
            3 => Self::Physync,
            other => Self::Unknown(other),
        }
    }

    fn to_raw(self) -> u32 {
        match self {
            Self::None => 0,
            Self::ExtTs => 1,
            Self::Perout => 2,
            Self::Physync => 3,
            Self::Unknown(raw) => raw,
        }
    }
}

/// A pin assignment to write with [`PtpClock::set_pin_function`].
///
/// ```
/// use ptp_clock::{PinFunc, PinFunction};
///
/// let assignment = PinFunction { index: 0, func: PinFunc::ExtTs, chan: 1 };
/// assert_eq!(assignment.func, PinFunc::ExtTs);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PinFunction {
    /// Pin index, below [`Capabilities::n_pins`](crate::Capabilities::n_pins).
    pub index: u32,
    /// Function to assign.
    pub func: PinFunc,
    /// Channel the function uses; ignored for [`PinFunc::None`].
    pub chan: u32,
}

/// A pin's current function and the driver-provided name.
///
/// ```
/// use ptp_clock::{PinDescription, PinFunc};
///
/// let pin = PinDescription {
///     name: Some("SMA1".into()),
///     index: 0,
///     func: PinFunc::Perout,
///     chan: 0,
/// };
/// assert_eq!(pin.name.as_deref(), Some("SMA1"));
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PinDescription {
    /// Human-readable name from the driver, when it provides one.
    pub name: Option<String>,
    /// Pin index.
    pub index: u32,
    /// Function currently assigned.
    pub func: PinFunc,
    /// Channel the function uses.
    pub chan: u32,
}

impl PtpClock {
    /// Reads the function assigned to a pin with `PTP_PIN_GETFUNC2`.
    ///
    /// `PTP_PIN_GETFUNC` is used on kernels without the V2 ioctl.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// for index in 0..clock.capabilities()?.n_pins {
    ///     let pin = clock.pin_function(index)?;
    ///     println!("{}: {:?} chan {}", pin.index, pin.func, pin.chan);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn pin_function(&self, index: u32) -> Result<PinDescription, Error> {
        let mut raw: RawPinDesc = zeroed();
        raw.index = index;
        self.dispatch_v2(
            Operation::PinGetFunction,
            IoctlRequest::PIN_GETFUNC2,
            IoctlRequest::PIN_GETFUNC,
            as_bytes_mut(&mut raw),
        )?;
        Ok(PinDescription {
            name: parse_name(&raw.name),
            index: raw.index,
            func: PinFunc::from_raw(raw.func),
            chan: raw.chan,
        })
    }

    /// Assigns a function to a pin with `PTP_PIN_SETFUNC2`.
    ///
    /// The kernel validates the channel against the function, rejects an
    /// invalid pin index with `EINVAL`, and reports `EOPNOTSUPP` when the
    /// driver's `verify` callback refuses the combination.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// use ptp_clock::{PinFunc, PinFunction};
    ///
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// clock.set_pin_function(&PinFunction { index: 0, func: PinFunc::ExtTs, chan: 0 })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_pin_function(&self, function: &PinFunction) -> Result<(), Error> {
        let mut raw: RawPinDesc = zeroed();
        raw.index = function.index;
        raw.func = function.func.to_raw();
        raw.chan = function.chan;
        self.dispatch_v2(
            Operation::PinSetFunction,
            IoctlRequest::PIN_SETFUNC2,
            IoctlRequest::PIN_SETFUNC,
            as_bytes_mut(&mut raw),
        )
    }
}

fn parse_name(raw: &[u8; 64]) -> Option<String> {
    let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
    if end == 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&raw[..end]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stop_at_the_first_nul_and_tolerate_non_utf8() {
        let mut raw = [0u8; 64];
        raw[..4].copy_from_slice(b"SMA1");
        assert_eq!(parse_name(&raw).as_deref(), Some("SMA1"));

        let raw = [0u8; 64];
        assert_eq!(parse_name(&raw), None);

        let mut raw = [0xffu8; 64];
        raw[1] = 0;
        assert!(parse_name(&raw).is_some());
    }

    #[test]
    fn unknown_functions_round_trip() {
        for func in [
            PinFunc::None,
            PinFunc::ExtTs,
            PinFunc::Perout,
            PinFunc::Physync,
        ] {
            assert_eq!(PinFunc::from_raw(func.to_raw()), func);
        }
        assert_eq!(PinFunc::from_raw(9), PinFunc::Unknown(9));
    }
}
