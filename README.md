# ptp-clock

[![Crates.io](https://img.shields.io/crates/v/ptp-clock.svg)](https://crates.io/crates/ptp-clock)
[![Documentation](https://docs.rs/ptp-clock/badge.svg)](https://docs.rs/ptp-clock)
[![License](https://img.shields.io/crates/l/ptp-clock.svg)](LICENSE)
[![CI](https://github.com/lenitain/ptp-clock/actions/workflows/ci.yml/badge.svg)](https://github.com/lenitain/ptp-clock/actions/workflows/ci.yml)

Complete mechanism layer for the Linux PTP hardware clock (PHC): the full `PTP_*` ioctl family and hardware timestamping.

The crate covers what `/dev/ptpN` and `SIOC[GS]HWTSTAMP` expose — capabilities, reading and steering the clock, system-offset sampling, external timestamps with an event queue, periodic outputs, pins, PPS, and per-interface timestamping configuration — with `repr(C)` layouts checked against the UAPI, V2-first ioctls that fall back to V1, RAII handles, and errors that name the failed operation. Policy stays with the caller: no event loop, no clock discovery, no servo algorithm; `PtpClock::as_fd` and `PtpClock::open_path` are the escape hatches.

```rust
use ptp_clock::{ExtTsRequest, PtpClock};

fn main() -> Result<(), ptp_clock::Error> {
    let clock = PtpClock::open(0)?;
    let caps = clock.capabilities()?;
    println!("max_adj={} ppb, ext_ts={}", caps.max_adj, caps.n_ext_ts);
    println!("PHC time: {}", clock.get_time()?);

    // The handle releases the channel when it goes out of scope.
    let _ext_ts = clock.request_ext_ts(&ExtTsRequest::rising(0))?;
    if let Some(event) = clock.read_event()? {
        println!("rising edge on channel {} at {}", event.index(), event.time());
    }
    Ok(())
}
```

Everything above needs a PHC and the matching permissions; `cargo run --example inspect` prints the same information for a device on the local machine.

## License

MIT
