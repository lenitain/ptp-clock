//! Inspect a PTP hardware clock: capabilities, time, and one external
//! timestamp event.
//!
//! Run as `cargo run --example inspect [index]` on a machine with a PHC and
//! permission to open `/dev/ptpN`.

use std::time::Duration;

use ptp_clock::{ExtTsRequest, PtpClock};

fn main() -> Result<(), ptp_clock::Error> {
    let index = std::env::args()
        .nth(1)
        .map(|arg| arg.parse().expect("clock index must be a number"))
        .unwrap_or(0);

    let clock = PtpClock::open(index)?;
    println!("/dev/ptp{index} (writable: {})", clock.is_writable());

    let caps = clock.capabilities()?;
    println!(
        "max_adj={} ppb, ext_ts={}, per_out={}, pins={}, pps={}, cross_timestamping={}",
        caps.max_adj, caps.n_ext_ts, caps.n_per_out, caps.n_pins, caps.pps, caps.cross_timestamping
    );
    println!("PHC time: {}", clock.get_time()?);

    if caps.adjust_phase {
        println!("phase adjustment: up to {} ns", caps.max_phase_adj);
    }

    if !caps.has_ext_ts(0) {
        return Ok(());
    }

    // The handle releases the channel when it goes out of scope.
    let _ext_ts = clock.request_ext_ts(&ExtTsRequest::rising(0))?;
    println!("waiting up to a second for a rising edge on channel 0…");

    // The escape hatch: wait for the event queue with poll(2).
    let mut poll_fd = libc::pollfd {
        fd: std::os::fd::AsRawFd::as_raw_fd(&clock.as_fd()),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll_fd is a valid pollfd for the lifetime of the call.
    let ready = unsafe { libc::poll(&mut poll_fd, 1, Duration::from_secs(1).as_millis() as i32) };
    if ready > 0
        && let Some(event) = clock.read_event()?
    {
        println!("edge on channel {} at {}", event.index(), event.time());
    } else {
        println!("no edge arrived");
    }

    Ok(())
}
