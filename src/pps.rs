//! Enabling and disabling the PPS signal.

use crate::clock::PtpClock;
use crate::error::{Error, Operation};
use crate::ioctl::IoctlRequest;

impl PtpClock {
    /// Enables or disables the PPS signal with `PTP_ENABLE_PPS2`.
    ///
    /// Requires [`Capabilities::pps`](crate::Capabilities::pps) and
    /// `CAP_SYS_TIME`; the kernel reports `EPERM` without the capability. The
    /// ioctl argument is the boolean itself, not a pointer, which the injected
    /// [`Ioctl::ioctl_value`](crate::Ioctl::ioctl_value) boundary mirrors.
    ///
    /// ```no_run
    /// # fn main() -> Result<(), ptp_clock::Error> {
    /// let clock = ptp_clock::PtpClock::open(0)?;
    /// if clock.capabilities()?.has_pps() {
    ///     clock.enable_pps(true)?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn enable_pps(&self, enabled: bool) -> Result<(), Error> {
        self.dispatch_value_v2(
            Operation::EnablePps,
            IoctlRequest::ENABLE_PPS2,
            IoctlRequest::ENABLE_PPS,
            if enabled { 1 } else { 0 },
        )
    }
}
