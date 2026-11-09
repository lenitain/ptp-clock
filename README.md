# ptp-clock

Complete mechanism layer for the Linux PTP hardware clock (PHC): the full PTP_* ioctl family and hardware timestamping.

> **Status: design stage.** The crate skeleton and API sketch exist; nothing is implemented yet.

## Design

The full design document lives in [DESIGN.md](DESIGN.md). It defines:

- **Scope (mechanism layer — must do):** what this crate implements completely and correctly.
- **Non-goals (policy layer — must not do):** what is deliberately left to the caller, so the crate never constrains user behavior.
- **API sketch, correctness concerns, and naming rationale.**

## Layout philosophy

This crate follows the same layering as [`fanotify-fid`](https://crates.io/crates/fanotify-fid): a complete mechanism layer
(protocol/parsing/correctness) with the policy layer (event loop, caching, strategy) left to the application.
