//! Sampling the offset between the system clock and the PHC.

use crate::clock::{ClockTime, PtpClock};
use crate::error::{Error, Operation};
use crate::ioctl::{
    IoctlRequest, MAX_SAMPLES, RawSysOffset, RawSysOffsetExtended, RawSysOffsetPrecise,
    as_bytes_mut, zeroed,
};

/// One system/PHC timestamp pair from [`SysOffset`].
///
/// The system timestamp is taken immediately before the PHC is read, so the
/// pair brackets one PHC access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SysOffsetSample {
    /// System clock reading before the PHC was read.
    pub sys: ClockTime,
    /// PHC reading.
    pub phc: ClockTime,
}

/// Interleaved system/PHC samples from `PTP_SYS_OFFSET2`.
///
/// The kernel returns `2 * n_samples + 1` timestamps laid out
/// `[sys, phc, sys, phc, …]`; the trailing extra system timestamp closes the
/// series and is exposed as [`SysOffset::sys_after`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SysOffset {
    /// The requested samples, in order.
    pub samples: Vec<SysOffsetSample>,
    /// System timestamp taken after the last PHC read.
    pub sys_after: ClockTime,
}

/// A precise system/PHC cross timestamp from `PTP_SYS_OFFSET_PRECISE2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SysOffsetPrecise {
    /// PHC timestamp.
    pub device: ClockTime,
    /// `CLOCK_REALTIME` timestamp taken atomically with the PHC reading.
    pub sys_realtime: ClockTime,
    /// `CLOCK_MONOTONIC_RAW` timestamp taken atomically with the PHC reading.
    pub sys_monoraw: ClockTime,
}

/// One pre/PHC/post triple from [`SysOffsetExtended`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SysOffsetExtendedSample {
    /// System timestamp taken before the PHC read.
    pub pre: ClockTime,
    /// PHC reading.
    pub phc: ClockTime,
    /// System timestamp taken after the PHC read.
    pub post: ClockTime,
}

/// Pre/PHC/post triples from `PTP_SYS_OFFSET_EXTENDED2`.
///
/// Bracketing each PHC read with two system reads bounds the error introduced
/// by the syscall path, which the plain [`SysOffset`] cannot do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SysOffsetExtended {
    /// The requested samples, in order.
    pub samples: Vec<SysOffsetExtendedSample>,
    /// The system clock the pre/post timestamps were taken from.
    pub clockid: libc::clockid_t,
}

impl PtpClock {
    /// Samples `n_samples` interleaved system/PHC pairs with `PTP_SYS_OFFSET2`.
    ///
    /// `n_samples` must not exceed 25 (`PTP_MAX_SAMPLES`); zero is accepted and
    /// yields only [`SysOffset::sys_after`].
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// for sample in clock.sys_offset(5)?.samples {
    ///     println!("{} vs {}", sample.sys, sample.phc);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn sys_offset(&self, n_samples: u32) -> Result<SysOffset, Error> {
        check_samples(n_samples, Operation::SysOffset)?;
        let mut raw: RawSysOffset = zeroed();
        raw.n_samples = n_samples;
        self.dispatch_v2(
            Operation::SysOffset,
            IoctlRequest::SYS_OFFSET2,
            IoctlRequest::SYS_OFFSET,
            as_bytes_mut(&mut raw),
        )?;
        Ok(SysOffset::from_raw(&raw, n_samples as usize))
    }

    /// Reads one precise cross timestamp with `PTP_SYS_OFFSET_PRECISE2`.
    ///
    /// Requires [`Capabilities::cross_timestamping`](crate::Capabilities::cross_timestamping);
    /// the kernel reports `EOPNOTSUPP` otherwise.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// let x = clock.sys_offset_precise()?;
    /// println!("device={} realtime={}", x.device, x.sys_realtime);
    /// # Ok(())
    /// # }
    /// ```
    pub fn sys_offset_precise(&self) -> Result<SysOffsetPrecise, Error> {
        let mut raw: RawSysOffsetPrecise = zeroed();
        self.dispatch_v2(
            Operation::SysOffsetPrecise,
            IoctlRequest::SYS_OFFSET_PRECISE2,
            IoctlRequest::SYS_OFFSET_PRECISE,
            as_bytes_mut(&mut raw),
        )?;
        Ok(SysOffsetPrecise {
            device: raw.device,
            sys_realtime: raw.sys_realtime,
            sys_monoraw: raw.sys_monoraw,
        })
    }

    /// Samples bracketed pre/PHC/post triples with `PTP_SYS_OFFSET_EXTENDED2`.
    ///
    /// `clockid` selects the system clock used for the bracketing timestamps
    /// and is honored by Linux 6.12 and later; older kernels ignore the field,
    /// so pass `CLOCK_REALTIME` (0) for compatibility. The kernel accepts
    /// `CLOCK_REALTIME`, `CLOCK_MONOTONIC`, `CLOCK_MONOTONIC_RAW`, and — where
    /// enabled — the auxiliary clocks.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// let samples = clock.sys_offset_extended(4, libc::CLOCK_MONOTONIC)?.samples;
    /// assert!(samples.len() <= 4);
    /// # Ok(())
    /// # }
    /// ```
    pub fn sys_offset_extended(
        &self,
        n_samples: u32,
        clockid: libc::clockid_t,
    ) -> Result<SysOffsetExtended, Error> {
        check_samples(n_samples, Operation::SysOffsetExtended)?;
        let mut raw: RawSysOffsetExtended = zeroed();
        raw.n_samples = n_samples;
        raw.clockid = clockid;
        self.dispatch_v2(
            Operation::SysOffsetExtended,
            IoctlRequest::SYS_OFFSET_EXTENDED2,
            IoctlRequest::SYS_OFFSET_EXTENDED,
            as_bytes_mut(&mut raw),
        )?;
        Ok(SysOffsetExtended::from_raw(
            &raw,
            n_samples as usize,
            clockid,
        ))
    }
}

fn check_samples(n_samples: u32, operation: Operation) -> Result<(), Error> {
    if n_samples as usize > MAX_SAMPLES {
        return Err(Error::invalid(
            operation,
            "n_samples exceeds PTP_MAX_SAMPLES (25)",
        ));
    }
    Ok(())
}

impl SysOffset {
    fn from_raw(raw: &RawSysOffset, n_samples: usize) -> Self {
        let samples = (0..n_samples)
            .map(|i| SysOffsetSample {
                sys: raw.ts[2 * i],
                phc: raw.ts[2 * i + 1],
            })
            .collect();
        Self {
            samples,
            sys_after: raw.ts[2 * n_samples],
        }
    }
}

impl SysOffsetExtended {
    fn from_raw(raw: &RawSysOffsetExtended, n_samples: usize, clockid: libc::clockid_t) -> Self {
        let samples = raw.ts[..n_samples]
            .iter()
            .map(|sample| SysOffsetExtendedSample {
                pre: sample[0],
                phc: sample[1],
                post: sample[2],
            })
            .collect();
        Self { samples, clockid }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(sec: i64, nsec: u32) -> ClockTime {
        ClockTime::new(sec, nsec)
    }

    #[test]
    fn plain_offset_parses_the_interleaved_array() {
        let mut raw: RawSysOffset = zeroed();
        raw.n_samples = 2;
        raw.ts[0] = time(10, 1);
        raw.ts[1] = time(11, 2);
        raw.ts[2] = time(12, 3);
        raw.ts[3] = time(13, 4);
        raw.ts[4] = time(14, 5);

        let parsed = SysOffset::from_raw(&raw, 2);
        assert_eq!(parsed.samples.len(), 2);
        assert_eq!(
            parsed.samples[0],
            SysOffsetSample {
                sys: ClockTime::new(10, 1),
                phc: ClockTime::new(11, 2),
            }
        );
        assert_eq!(
            parsed.samples[1],
            SysOffsetSample {
                sys: ClockTime::new(12, 3),
                phc: ClockTime::new(13, 4),
            }
        );
        assert_eq!(parsed.sys_after, ClockTime::new(14, 5));
    }

    #[test]
    fn zero_samples_still_yields_the_trailing_system_timestamp() {
        let mut raw: RawSysOffset = zeroed();
        raw.ts[0] = time(-1, 999_999_999);
        let parsed = SysOffset::from_raw(&raw, 0);
        assert!(parsed.samples.is_empty());
        assert_eq!(parsed.sys_after, ClockTime::new(-1, 999_999_999));
    }

    #[test]
    fn extended_parses_each_triple_in_order() {
        let mut raw: RawSysOffsetExtended = zeroed();
        raw.n_samples = 2;
        raw.ts[0] = [time(1, 0), time(2, 0), time(3, 0)];
        raw.ts[1] = [time(4, 0), time(5, 0), time(6, 0)];
        let parsed = SysOffsetExtended::from_raw(&raw, 2, libc::CLOCK_MONOTONIC);
        assert_eq!(parsed.samples.len(), 2);
        assert_eq!(
            parsed.samples[1],
            SysOffsetExtendedSample {
                pre: ClockTime::new(4, 0),
                phc: ClockTime::new(5, 0),
                post: ClockTime::new(6, 0),
            }
        );
        assert_eq!(parsed.clockid, libc::CLOCK_MONOTONIC);
    }

    #[test]
    fn sample_count_is_bounded_by_the_uapi() {
        let err = check_samples(26, Operation::SysOffset).unwrap_err();
        assert_eq!(err.kind(), crate::ErrorKind::InvalidArgument);
        assert!(check_samples(25, Operation::SysOffset).is_ok());
    }
}
