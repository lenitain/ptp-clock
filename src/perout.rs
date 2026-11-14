//! Periodic output channels and their RAII handles.

use crate::clock::{ClockTime, PtpClock};
use crate::error::{Error, Operation};
use crate::ioctl::{IoctlRequest, RawPerOut, as_bytes_mut, zeroed};

const PTP_PEROUT_ONE_SHOT: u32 = 1 << 0;
const PTP_PEROUT_DUTY_CYCLE: u32 = 1 << 1;
const PTP_PEROUT_PHASE: u32 = 1 << 2;

/// Where a periodic output starts.
///
/// The kernel overlays these two meanings on the same request field, selected
/// by `PTP_PEROUT_PHASE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PeroutStart {
    /// An absolute PHC time at which the signal starts.
    ///
    /// The kernel aligns the start to the signal's period, so any start time in
    /// the same period produces the same phase.
    Absolute(ClockTime),
    /// An offset within the period, starting as soon as possible.
    ///
    /// The phase must be smaller than the period; the kernel rejects anything
    /// equal or larger with `ERANGE`.
    Phase(ClockTime),
}

/// Configuration for one periodic output channel.
///
/// A zero [`period`](Self::period) disables the channel.
///
/// ```
/// use ptp_clock::{ClockTime, PeroutRequest, PeroutStart};
///
/// let req = PeroutRequest {
///     index: 0,
///     period: ClockTime::from_nanos(1_000_000_000),
///     start: PeroutStart::Phase(ClockTime::from_nanos(250_000_000)),
///     duty_cycle: Some(ClockTime::from_nanos(500_000_000)),
///     one_shot: false,
/// };
/// assert_eq!(req.period.secs(), 1);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PeroutRequest {
    /// Channel index, below [`Capabilities::n_per_out`](crate::Capabilities::n_per_out).
    pub index: u32,
    /// Signal period; zero disables the channel.
    pub period: ClockTime,
    /// Absolute start time or in-period phase.
    pub start: PeroutStart,
    /// High time of each period, smaller than the period.
    ///
    /// Requires `PTP_PEROUT_REQUEST2`; the V1 ioctl has no duty-cycle support.
    pub duty_cycle: Option<ClockTime>,
    /// Emit a single pulse instead of a continuous signal.
    ///
    /// Requires `PTP_PEROUT_REQUEST2`.
    pub one_shot: bool,
}

impl PeroutRequest {
    /// A continuous signal with the given period and start.
    pub fn new(index: u32, period: ClockTime, start: PeroutStart) -> Self {
        Self {
            index,
            period,
            start,
            duty_cycle: None,
            one_shot: false,
        }
    }
}

/// An enabled periodic output channel.
///
/// Dropping the handle disables the channel by writing the request with a zero
/// period. Release errors are ignored in `drop`; use
/// [`release`](Self::release) when the result matters, or
/// [`disarm`](Self::disarm) to leave the signal running.
///
/// ```no_run
/// # fn main() -> Result<(), ptp_clock::Error> {
/// use ptp_clock::{ClockTime, PeroutRequest, PeroutStart};
///
/// let clock = ptp_clock::PtpClock::open(0)?;
/// let req = PeroutRequest::new(
///     0,
///    ClockTime::from_nanos(1_000_000),
///     PeroutStart::Absolute(ClockTime::ZERO),
/// );
/// let _handle = clock.request_perout(&req)?;
/// // … the signal is disabled here …
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct PeroutHandle<'a> {
    clock: &'a PtpClock,
    index: u32,
    active: bool,
}

impl PeroutHandle<'_> {
    /// The channel index this handle configured.
    pub fn index(&self) -> u32 {
        self.index
    }

    /// Whether the handle enabled a signal (a zero period is a no-op).
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Disables the channel, reporting any failure.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// # use ptp_clock::{ClockTime, PeroutRequest, PeroutStart};
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// let req = PeroutRequest::new(0, ClockTime::from_nanos(1_000_000), PeroutStart::Absolute(ClockTime::ZERO));
    /// clock.request_perout(&req)?.release()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn release(mut self) -> Result<(), Error> {
        self.active = false;
        self.disable()
    }

    /// Leaves the signal running and gives up the handle.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// # use ptp_clock::{ClockTime, PeroutRequest, PeroutStart};
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// let req = PeroutRequest::new(0, ClockTime::from_nanos(1_000_000), PeroutStart::Absolute(ClockTime::ZERO));
    /// clock.request_perout(&req)?.disarm();
    /// # Ok(())
    /// # }
    /// ```
    pub fn disarm(mut self) {
        self.active = false;
    }

    fn disable(&self) -> Result<(), Error> {
        let mut raw: RawPerOut = zeroed();
        raw.index = self.index;
        self.clock.dispatch_v2(
            Operation::PerOut,
            IoctlRequest::PEROUT_REQUEST2,
            IoctlRequest::PEROUT_REQUEST,
            as_bytes_mut(&mut raw),
        )
    }
}

impl Drop for PeroutHandle<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.disable();
        }
    }
}

impl PtpClock {
    /// Configures a periodic output channel.
    ///
    /// `PTP_PEROUT_REQUEST2` is tried first. Kernels without it accept only
    /// `PTP_PEROUT_REQUEST`, which cannot express a phase start, a duty cycle,
    /// or one-shot mode; such requests fail instead of silently changing
    /// meaning. A zero period disables the channel.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// use ptp_clock::{ClockTime, PeroutRequest, PeroutStart};
    ///
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// let req = PeroutRequest {
    ///     index: 0,
    ///     period: ClockTime::from_nanos(1_000_000_000),
    ///     start: PeroutStart::Absolute(ClockTime::ZERO),
    ///     duty_cycle: Some(ClockTime::from_nanos(500_000_000)),
    ///     one_shot: false,
    /// };
    /// let _handle = clock.request_perout(&req)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn request_perout(&self, request: &PeroutRequest) -> Result<PeroutHandle<'_>, Error> {
        if request.duty_cycle.is_some() && request.one_shot {
            return Err(Error::invalid(
                Operation::PerOut,
                "a one-shot pulse cannot have a duty cycle",
            ));
        }

        let mut raw: RawPerOut = zeroed();
        raw.index = request.index;
        raw.period = request.period;
        match request.start {
            PeroutStart::Absolute(time) => raw.start = time,
            PeroutStart::Phase(phase) => {
                raw.start = phase;
                raw.flags |= PTP_PEROUT_PHASE;
            }
        }
        if let Some(duty) = request.duty_cycle {
            raw.on = duty;
            raw.flags |= PTP_PEROUT_DUTY_CYCLE;
        }
        if request.one_shot {
            raw.flags |= PTP_PEROUT_ONE_SHOT;
        }

        match self.dispatch(
            Operation::PerOut,
            IoctlRequest::PEROUT_REQUEST2,
            as_bytes_mut(&mut raw),
        ) {
            Ok(()) => Ok(PeroutHandle {
                clock: self,
                index: request.index,
                active: !request.period.is_zero(),
            }),
            Err(Error::Ioctl { source, .. }) if source.raw_os_error() == Some(libc::ENOTTY) => {
                if raw.flags != 0 {
                    return Err(Error::invalid(
                        Operation::PerOut,
                        "this kernel only supports PTP_PEROUT_REQUEST, which cannot express a phase start, duty cycle, or one-shot mode",
                    ));
                }
                self.dispatch(
                    Operation::PerOut,
                    IoctlRequest::PEROUT_REQUEST,
                    as_bytes_mut(&mut raw),
                )?;
                Ok(PeroutHandle {
                    clock: self,
                    index: request.index,
                    active: !request.period.is_zero(),
                })
            }
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_fields_land_in_the_overlaid_uapi_slots() {
        let mut raw: RawPerOut = zeroed();
        raw.period = ClockTime::from_nanos(4);
        raw.start = ClockTime::from_nanos(5);
        raw.on = ClockTime::from_nanos(6);
        assert_eq!(raw.start, ClockTime::new(0, 5));
        assert_eq!(raw.period.nanos(), 4);
        assert_eq!(raw.on.nanos(), 6);
        // The struct has no holes: the union overlays are flattened.
        assert_eq!(core::mem::size_of::<RawPerOut>(), 56);
    }

    #[test]
    fn period_zero_is_a_disable_request() {
        let req = PeroutRequest::new(0, ClockTime::ZERO, PeroutStart::Absolute(ClockTime::ZERO));
        assert!(req.period.is_zero());
        assert_eq!(req.period, ClockTime::ZERO);
    }
}
