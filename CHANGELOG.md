# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-14

### Added

- `PtpClock::open` / `open_path` / `from_fd` with read-write then read-only fallback, plus `as_fd` as the escape hatch.
- Typed capabilities (`PTP_CLOCK_GETCAPS2`, V1 fallback) and capability predicates.
- Time access and steering via the dynamic POSIX clock fd: `get_time`, `set_time`, `adjust_freq`, `frequency_ppb`, `adjust_phase`, `step_time`.
- System offset sampling: `sys_offset`, `sys_offset_precise`, `sys_offset_extended`.
- External timestamp channels with RAII handles and event queue reads (`read_event`, `read_events`).
- Periodic outputs with RAII handles.
- Pin function discovery and assignment.
- PPS enablement.
- `SIOC[GS]HWTSTAMP` configuration that reports the filter the driver actually enabled.
- Injectable `Ioctl` boundary, typed errors (`Error`, `ErrorKind`, `Operation`) and V2→V1 fallback for every ioctl.
