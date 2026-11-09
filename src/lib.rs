//! Complete mechanism layer for the Linux PTP hardware clock (PHC): the full PTP_* ioctl family and hardware timestamping.
//!
//! **Design stage** — see [`DESIGN.md`](https://github.com/lenitain/ptp-clock/blob/main/DESIGN.md)
//! for the full design document (scope, non-goals, API sketch, correctness concerns).
//!
//! The public API below is a placeholder; nothing is implemented yet.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

#[cfg(not(target_os = "linux"))]
compile_error!("ptp-clock only supports Linux");

/// Placeholder module. The real API will live here once the design is implemented.
pub mod api {
    /// Marker type reserved for the mechanism-layer API.
    #[derive(Debug)]
    pub struct Placeholder;
}

#[cfg(test)]
mod tests {
    #[test]
    fn placeholder_compiles() {
        assert_eq!(2 + 2, 4);
    }
}
