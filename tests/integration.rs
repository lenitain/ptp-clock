//! Integration tests: the public API driven against a synthetic kernel.
//!
//! Every PTP ioctl goes through [`Ioctl`], so success paths, V2→V1 fallbacks,
//! and error paths all run on a machine without a PHC. The only test that
//! reaches real kernel code is the loopback `SIOCGHWTSTAMP` error path, which
//! needs no PTP device.

use std::ffi::{c_ulong, c_void};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use ptp_clock::{
    Capabilities, ClockTime, Error, ErrorKind, ExtTsEvent, ExtTsFlags, ExtTsRequest, HwRxFilter,
    HwTimestampConfig, HwTimestamping, HwTxType, Ioctl, IoctlRequest, Operation, PeroutRequest,
    PeroutStart, PinFunc, PinFunction, PtpClock, SysOffset, SysOffsetSample,
};

// ---------------------------------------------------------------------------
// UAPI mirrors. These fixtures are written independently of the crate's private
// raw structures, so the tests check the wire format instead of echoing it.
// ---------------------------------------------------------------------------

const PTP_ENABLE_FEATURE: u32 = 1 << 0;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RawTime {
    sec: i64,
    nsec: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct RawCaps {
    max_adj: i32,
    n_alarm: i32,
    n_ext_ts: i32,
    n_per_out: i32,
    pps: i32,
    n_pins: i32,
    cross_timestamping: i32,
    adjust_phase: i32,
    max_phase_adj: i32,
    rsv: [i32; 11],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct RawExtTsRequest {
    index: u32,
    flags: u32,
    rsv: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct RawPerOut {
    start: RawTime,
    period: RawTime,
    index: u32,
    flags: u32,
    on: RawTime,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawSysOffset {
    n_samples: u32,
    rsv: [u32; 3],
    ts: [RawTime; 51],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RawSysOffsetPrecise {
    device: RawTime,
    sys_realtime: RawTime,
    sys_monoraw: RawTime,
    rsv: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawSysOffsetExtended {
    n_samples: u32,
    clockid: i32,
    rsv: [u32; 2],
    ts: [[RawTime; 3]; 25],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RawPinDesc {
    name: [u8; 64],
    index: u32,
    func: u32,
    chan: u32,
    rsv: [u32; 5],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct RawExtTsEvent {
    t: RawTime,
    index: u32,
    flags: u32,
    rsv: [u32; 2],
}

/// The leading fields of `struct ifreq`; the crate passes a 48-byte buffer.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawIfReq {
    name: [u8; 16],
    data: *mut c_void,
    tail: [u8; 24],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RawHwTimestamp {
    flags: i32,
    tx_type: i32,
    rx_filter: i32,
}

fn write_at<T: Copy>(buffer: &mut [u8], value: &T) {
    assert!(buffer.len() >= std::mem::size_of::<T>());
    // SAFETY: the buffer is large enough and T is Copy.
    unsafe { buffer.as_mut_ptr().cast::<T>().write_unaligned(*value) };
}

fn read_at<T: Copy>(buffer: &[u8]) -> T {
    assert!(buffer.len() >= std::mem::size_of::<T>());
    // SAFETY: the buffer is large enough and T is Copy.
    unsafe { buffer.as_ptr().cast::<T>().read_unaligned() }
}

// ---------------------------------------------------------------------------
// Synthetic kernel
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Call {
    request: IoctlRequest,
    buffer: Vec<u8>,
    value: Option<c_ulong>,
}

type Handler = Box<dyn Fn(IoctlRequest, &mut [u8]) -> io::Result<()> + Send + Sync>;

struct Fake {
    calls: Mutex<Vec<Call>>,
    handler: Handler,
}

impl Fake {
    fn new(
        handler: impl Fn(IoctlRequest, &mut [u8]) -> io::Result<()> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            handler: Box::new(handler),
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

impl Ioctl for Fake {
    fn ioctl(
        &self,
        _fd: std::os::fd::BorrowedFd<'_>,
        request: IoctlRequest,
        buffer: &mut [u8],
    ) -> io::Result<()> {
        self.calls.lock().unwrap().push(Call {
            request,
            buffer: buffer.to_vec(),
            value: None,
        });
        (self.handler)(request, buffer)
    }

    fn ioctl_value(
        &self,
        _fd: std::os::fd::BorrowedFd<'_>,
        request: IoctlRequest,
        value: c_ulong,
    ) -> io::Result<()> {
        self.calls.lock().unwrap().push(Call {
            request,
            buffer: Vec::new(),
            value: Some(value),
        });
        Ok(())
    }
}

struct Shared(Arc<Fake>);

impl Ioctl for Shared {
    fn ioctl(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
        request: IoctlRequest,
        buffer: &mut [u8],
    ) -> io::Result<()> {
        self.0.ioctl(fd, request, buffer)
    }

    fn ioctl_value(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
        request: IoctlRequest,
        value: c_ulong,
    ) -> io::Result<()> {
        self.0.ioctl_value(fd, request, value)
    }
}

fn null_fd() -> OwnedFd {
    File::open("/dev/null").unwrap().into()
}

/// A clock backed by an arbitrary fd and a synthetic kernel.
fn clock_with_fd(fd: impl Into<OwnedFd>, fake: &Arc<Fake>) -> PtpClock {
    PtpClock::from_fd(fd.into(), Box::new(Shared(Arc::clone(fake))))
}

fn clock_with(
    handler: impl Fn(IoctlRequest, &mut [u8]) -> io::Result<()> + Send + Sync + 'static,
) -> (PtpClock, Arc<Fake>) {
    let fake = Fake::new(handler);
    let clock = clock_with_fd(null_fd(), &fake);
    (clock, fake)
}

fn errno(errno: i32) -> io::Error {
    io::Error::from_raw_os_error(errno)
}

fn sample_caps() -> RawCaps {
    RawCaps {
        max_adj: 100_000_000,
        n_alarm: 0,
        n_ext_ts: 2,
        n_per_out: 1,
        pps: 1,
        n_pins: 3,
        cross_timestamping: 1,
        adjust_phase: 1,
        max_phase_adj: 500,
        rsv: [0; 11],
    }
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

#[test]
fn capabilities_read_the_v2_response() {
    let (clock, fake) = clock_with(|request, buffer| {
        assert_eq!(request, IoctlRequest::CLOCK_GETCAPS2);
        write_at(buffer, &sample_caps());
        Ok(())
    });

    let caps = clock.capabilities().unwrap();
    assert_eq!(
        caps,
        Capabilities {
            max_adj: 100_000_000,
            n_alarm: 0,
            n_ext_ts: 2,
            n_per_out: 1,
            pps: true,
            n_pins: 3,
            cross_timestamping: true,
            adjust_phase: true,
            max_phase_adj: 500,
        }
    );
    assert!(caps.has_ext_ts(1));
    assert!(!caps.has_ext_ts(2));
    assert!(caps.has_adjust_phase());
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn capabilities_fall_back_to_v1_on_enotty() {
    let (clock, fake) = clock_with(|request, buffer| match request {
        IoctlRequest::CLOCK_GETCAPS2 => Err(errno(libc::ENOTTY)),
        IoctlRequest::CLOCK_GETCAPS => {
            write_at(buffer, &sample_caps());
            Ok(())
        }
        other => panic!("unexpected request {other:?}"),
    });

    assert_eq!(clock.capabilities().unwrap().n_ext_ts, 2);
    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].request, IoctlRequest::CLOCK_GETCAPS2);
    assert_eq!(calls[1].request, IoctlRequest::CLOCK_GETCAPS);
}

#[test]
fn every_errno_maps_to_a_kind() {
    let cases = [
        (libc::ENOTTY, ErrorKind::NotSupported),
        (libc::EOPNOTSUPP, ErrorKind::NotSupported),
        (libc::EINVAL, ErrorKind::InvalidArgument),
        (libc::ERANGE, ErrorKind::InvalidArgument),
        (libc::EBUSY, ErrorKind::Busy),
        (libc::EPERM, ErrorKind::PermissionDenied),
        (libc::EACCES, ErrorKind::PermissionDenied),
        (libc::ENODEV, ErrorKind::NotFound),
        (libc::EIO, ErrorKind::Other),
    ];
    for (errno_value, kind) in cases {
        let (clock, _) = clock_with(move |_, _| Err(errno(errno_value)));
        let err = clock.capabilities().unwrap_err();
        assert_eq!(err.kind(), kind, "errno {errno_value}");
        assert_eq!(err.operation(), Operation::Capabilities);
        // The last attempt is the V1 fallback, which is what was reported.
        assert!(err.to_string().contains("PTP_CLOCK_GETCAPS"));
    }
}

#[test]
fn a_read_only_device_is_reported_as_such() {
    let (clock, _) = clock_with(|_, _| Ok(()));
    assert!(!clock.is_writable());
    assert!(clock.as_fd().as_raw_fd() >= 0);
}

#[test]
fn opening_a_missing_device_is_a_typed_error() {
    let err = PtpClock::open_path("/nonexistent/ptp0").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(err.operation(), Operation::Open);
    assert!(err.to_string().contains("/nonexistent/ptp0"));
    assert!(std::error::Error::source(&err).is_some());
}

// ---------------------------------------------------------------------------
// External timestamps
// ---------------------------------------------------------------------------

#[test]
fn ext_ts_enables_on_request_and_disables_on_drop() {
    let (clock, fake) = clock_with(|_, _| Ok(()));
    {
        let handle = clock.request_ext_ts(&ExtTsRequest::rising(1)).unwrap();
        assert_eq!(handle.index(), 1);
        assert!(handle.flags().contains(ExtTsFlags::RISING_EDGE));
    }

    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].request, IoctlRequest::EXTTS_REQUEST2);
    assert_eq!(calls[1].request, IoctlRequest::EXTTS_REQUEST2);
    let enabled: RawExtTsRequest = read_at(&calls[0].buffer);
    let disabled: RawExtTsRequest = read_at(&calls[1].buffer);
    assert_eq!(enabled.index, 1);
    assert_eq!(enabled.flags & PTP_ENABLE_FEATURE, PTP_ENABLE_FEATURE);
    assert_eq!(disabled.index, 1);
    assert_eq!(disabled.flags & PTP_ENABLE_FEATURE, 0);
    assert_eq!((enabled.rsv, disabled.rsv), ([0; 2], [0; 2]));
}

#[test]
fn ext_ts_falls_back_to_v1_for_plain_edges() {
    let (clock, fake) = clock_with(|request, _| match request {
        IoctlRequest::EXTTS_REQUEST2 => Err(errno(libc::ENOTTY)),
        IoctlRequest::EXTTS_REQUEST => Ok(()),
        other => panic!("unexpected request {other:?}"),
    });

    clock
        .request_ext_ts(&ExtTsRequest::rising(0))
        .unwrap()
        .release()
        .unwrap();
    let requests: Vec<_> = fake.calls().iter().map(|call| call.request).collect();
    assert_eq!(
        requests,
        vec![
            IoctlRequest::EXTTS_REQUEST2,
            IoctlRequest::EXTTS_REQUEST,
            IoctlRequest::EXTTS_REQUEST2,
            IoctlRequest::EXTTS_REQUEST,
        ]
    );
}

#[test]
fn ext_ts_ext_offset_needs_v2_instead_of_being_dropped() {
    let (clock, fake) = clock_with(|request, _| match request {
        IoctlRequest::EXTTS_REQUEST2 => Err(errno(libc::ENOTTY)),
        other => panic!("V1 must not be attempted, got {other:?}"),
    });

    let request = ExtTsRequest {
        index: 0,
        flags: ExtTsFlags::RISING_EDGE | ExtTsFlags::EXT_OFFSET,
    };
    let err = clock.request_ext_ts(&request).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert!(err.to_string().contains("PTP_EXT_OFFSET"));
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn ext_ts_release_reports_failures_instead_of_panicking() {
    let first = Arc::new(Mutex::new(true));
    let is_first = Arc::clone(&first);
    let (clock, _) = clock_with(move |_, _| {
        let mut first = is_first.lock().unwrap();
        let result = if *first {
            Ok(())
        } else {
            Err(errno(libc::EBUSY))
        };
        *first = false;
        result
    });

    let handle = clock.request_ext_ts(&ExtTsRequest::rising(0)).unwrap();
    let err = handle.release().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Busy);
    assert_eq!(err.operation(), Operation::ExtTs);
}

#[test]
fn ext_ts_disarm_leaves_the_channel_enabled() {
    let (clock, fake) = clock_with(|_, _| Ok(()));
    clock
        .request_ext_ts(&ExtTsRequest::rising(0))
        .unwrap()
        .disarm();
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn the_clock_can_be_shared_across_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PtpClock>();
    assert_send_sync::<HwTimestamping>();
}

#[test]
fn read_events_returns_parsed_events_and_none_when_empty() {
    let (mut writer, reader) = UnixStream::pair().unwrap();
    reader.set_nonblocking(true).unwrap();
    let fake = Fake::new(|_, _| Ok(()));
    let clock = clock_with_fd(reader, &fake);

    // An empty queue yields no events rather than an error.
    assert_eq!(clock.read_event().unwrap(), None);
    let mut buffer = [ExtTsEvent::default(); 4];
    assert_eq!(clock.read_events(&mut buffer).unwrap(), 0);

    let events = [
        RawExtTsEvent {
            t: RawTime {
                sec: 7,
                nsec: 8,
                reserved: 0,
            },
            index: 2,
            flags: 2,
            rsv: [0; 2],
        },
        RawExtTsEvent {
            t: RawTime {
                sec: -1,
                nsec: 9,
                reserved: 0,
            },
            index: 0,
            flags: 4,
            rsv: [0; 2],
        },
    ];
    let bytes = unsafe {
        std::slice::from_raw_parts(events.as_ptr().cast::<u8>(), std::mem::size_of_val(&events))
    };
    std::io::Write::write_all(&mut writer, bytes).unwrap();

    let count = clock.read_events(&mut buffer).unwrap();
    assert_eq!(count, 2);
    assert_eq!(buffer[0].time(), ClockTime::new(7, 8));
    assert_eq!(buffer[0].index(), 2);
    assert!(buffer[0].flags().contains(ExtTsFlags::RISING_EDGE));
    assert_eq!(buffer[0].reserved(), [0; 2]);
    assert_eq!(buffer[1].time(), ClockTime::new(-1, 9));
    assert!(buffer[1].flags().contains(ExtTsFlags::FALLING_EDGE));

    std::io::Write::write_all(&mut writer, &bytes[..32]).unwrap();
    let event = clock.read_event().unwrap().expect("one event");
    assert_eq!(event.time(), ClockTime::new(7, 8));
    assert_eq!(event.index(), 2);
}

// ---------------------------------------------------------------------------
// Periodic output
// ---------------------------------------------------------------------------

#[test]
fn perout_rejects_features_v1_cannot_express() {
    let (clock, fake) = clock_with(|request, _| match request {
        IoctlRequest::PEROUT_REQUEST2 => Err(errno(libc::ENOTTY)),
        other => panic!("V1 must not be attempted, got {other:?}"),
    });

    let request = PeroutRequest {
        index: 0,
        period: ClockTime::from_nanos(1_000_000),
        start: PeroutStart::Phase(ClockTime::from_nanos(10)),
        duty_cycle: None,
        one_shot: false,
    };
    let err = clock.request_perout(&request).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert!(err.to_string().contains("phase"));
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn perout_falls_back_to_v1_when_the_request_fits() {
    let (clock, fake) = clock_with(|request, _| match request {
        IoctlRequest::PEROUT_REQUEST2 => Err(errno(libc::ENOTTY)),
        IoctlRequest::PEROUT_REQUEST => Ok(()),
        other => panic!("unexpected {other:?}"),
    });

    let request = PeroutRequest::new(
        0,
        ClockTime::from_nanos(1_000_000),
        PeroutStart::Absolute(ClockTime::ZERO),
    );
    let handle = clock.request_perout(&request).unwrap();
    assert!(handle.is_active());
    handle.release().unwrap();

    let calls = fake.calls();
    assert_eq!(calls[0].request, IoctlRequest::PEROUT_REQUEST2);
    assert_eq!(calls[1].request, IoctlRequest::PEROUT_REQUEST);
    let enable: RawPerOut = read_at(&calls[1].buffer);
    assert_eq!((enable.period.sec, enable.period.nsec), (0, 1_000_000));
    // release() disables through the same V2→V1 path.
    let disable: RawPerOut = read_at(&calls.last().unwrap().buffer);
    assert_eq!((disable.period.sec, disable.period.nsec), (0, 0));
    assert_eq!(calls.last().unwrap().request, IoctlRequest::PEROUT_REQUEST);
}

#[test]
fn perout_drop_writes_a_zero_period_disable() {
    let (clock, fake) = clock_with(|_, _| Ok(()));
    let request = PeroutRequest {
        index: 3,
        period: ClockTime::from_nanos(2_000_000),
        start: PeroutStart::Phase(ClockTime::from_nanos(1)),
        duty_cycle: Some(ClockTime::from_nanos(1_000_000)),
        one_shot: false,
    };
    drop(clock.request_perout(&request).unwrap());

    let calls = fake.calls();
    assert_eq!(calls.len(), 2);
    let enable: RawPerOut = read_at(&calls[0].buffer);
    let disable: RawPerOut = read_at(&calls[1].buffer);
    assert_eq!(enable.index, 3);
    assert_eq!(enable.flags, 0b110);
    assert_eq!(enable.period.nsec, 2_000_000);
    assert_eq!(enable.on.nsec, 1_000_000);
    assert_eq!(enable.start.nsec, 1);
    assert_eq!(disable.index, 3);
    assert_eq!(disable.flags, 0);
    assert_eq!((disable.period.sec, disable.period.nsec), (0, 0));
}

#[test]
fn perout_rejects_a_one_shot_with_duty_cycle() {
    let (clock, fake) = clock_with(|_, _| Ok(()));
    let request = PeroutRequest {
        index: 0,
        period: ClockTime::from_nanos(10),
        start: PeroutStart::Absolute(ClockTime::ZERO),
        duty_cycle: Some(ClockTime::from_nanos(5)),
        one_shot: true,
    };
    let err = clock.request_perout(&request).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert!(fake.calls().is_empty());
}

// ---------------------------------------------------------------------------
// System offsets
// ---------------------------------------------------------------------------

#[test]
fn sys_offset_parses_the_interleaved_array() {
    let (clock, fake) = clock_with(|request, buffer| {
        assert_eq!(request, IoctlRequest::SYS_OFFSET2);
        let mut raw: RawSysOffset = read_at(buffer);
        assert_eq!(raw.n_samples, 2);
        assert_eq!(raw.rsv, [0; 3]);
        raw.ts[0] = RawTime {
            sec: 10,
            nsec: 1,
            reserved: 0,
        };
        raw.ts[1] = RawTime {
            sec: 11,
            nsec: 2,
            reserved: 0,
        };
        raw.ts[2] = RawTime {
            sec: 12,
            nsec: 3,
            reserved: 0,
        };
        raw.ts[3] = RawTime {
            sec: 13,
            nsec: 4,
            reserved: 0,
        };
        raw.ts[4] = RawTime {
            sec: 14,
            nsec: 5,
            reserved: 0,
        };
        write_at(buffer, &raw);
        Ok(())
    });

    let offset = clock.sys_offset(2).unwrap();
    assert_eq!(
        offset,
        SysOffset {
            samples: vec![
                SysOffsetSample {
                    sys: ClockTime::new(10, 1),
                    phc: ClockTime::new(11, 2)
                },
                SysOffsetSample {
                    sys: ClockTime::new(12, 3),
                    phc: ClockTime::new(13, 4)
                },
            ],
            sys_after: ClockTime::new(14, 5),
        }
    );
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn sys_offset_rejects_too_many_samples_before_the_syscall() {
    let (clock, fake) = clock_with(|_, _| Ok(()));
    let err = clock.sys_offset(26).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert_eq!(err.operation(), Operation::SysOffset);
    assert!(fake.calls().is_empty());
}

#[test]
fn sys_offset_precise_reads_all_three_timestamps() {
    let (clock, fake) = clock_with(|request, buffer| {
        assert_eq!(request, IoctlRequest::SYS_OFFSET_PRECISE2);
        write_at(
            buffer,
            &RawSysOffsetPrecise {
                device: RawTime {
                    sec: 1,
                    nsec: 2,
                    reserved: 0,
                },
                sys_realtime: RawTime {
                    sec: 3,
                    nsec: 4,
                    reserved: 0,
                },
                sys_monoraw: RawTime {
                    sec: 5,
                    nsec: 6,
                    reserved: 0,
                },
                rsv: [0; 4],
            },
        );
        Ok(())
    });

    let precise = clock.sys_offset_precise().unwrap();
    assert_eq!(precise.device, ClockTime::new(1, 2));
    assert_eq!(precise.sys_realtime, ClockTime::new(3, 4));
    assert_eq!(precise.sys_monoraw, ClockTime::new(5, 6));
    assert_eq!(fake.calls()[0].request, IoctlRequest::SYS_OFFSET_PRECISE2);
}

#[test]
fn sys_offset_extended_parses_triples_and_forwards_the_clockid() {
    let (clock, fake) = clock_with(|request, buffer| {
        assert_eq!(request, IoctlRequest::SYS_OFFSET_EXTENDED2);
        let mut raw: RawSysOffsetExtended = read_at(buffer);
        assert_eq!(raw.n_samples, 1);
        assert_eq!(raw.clockid, libc::CLOCK_MONOTONIC);
        assert_eq!(raw.rsv, [0; 2]);
        raw.ts[0] = [
            RawTime {
                sec: 1,
                nsec: 0,
                reserved: 0,
            },
            RawTime {
                sec: 2,
                nsec: 0,
                reserved: 0,
            },
            RawTime {
                sec: 3,
                nsec: 0,
                reserved: 0,
            },
        ];
        write_at(buffer, &raw);
        Ok(())
    });

    let extended = clock.sys_offset_extended(1, libc::CLOCK_MONOTONIC).unwrap();
    assert_eq!(extended.clockid, libc::CLOCK_MONOTONIC);
    assert_eq!(
        extended.samples,
        vec![ptp_clock::SysOffsetExtendedSample {
            pre: ClockTime::new(1, 0),
            phc: ClockTime::new(2, 0),
            post: ClockTime::new(3, 0),
        }]
    );
    assert_eq!(fake.calls().len(), 1);
}

// ---------------------------------------------------------------------------
// Pins and PPS
// ---------------------------------------------------------------------------

#[test]
fn pin_function_reads_names_and_preserves_unknown_functions() {
    let (clock, fake) = clock_with(|request, buffer| {
        assert_eq!(request, IoctlRequest::PIN_GETFUNC2);
        let mut raw: RawPinDesc = read_at(buffer);
        assert_eq!(raw.index, 1);
        raw.name[..5].copy_from_slice(b"SMA1\0");
        raw.func = 9;
        raw.chan = 4;
        write_at(buffer, &raw);
        Ok(())
    });

    let pin = clock.pin_function(1).unwrap();
    assert_eq!(pin.name.as_deref(), Some("SMA1"));
    assert_eq!(pin.index, 1);
    assert_eq!(pin.func, PinFunc::Unknown(9));
    assert_eq!(pin.chan, 4);
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn pin_assignment_writes_the_uapi_fields() {
    let (clock, fake) = clock_with(|_, _| Ok(()));
    clock
        .set_pin_function(&PinFunction {
            index: 2,
            func: PinFunc::ExtTs,
            chan: 1,
        })
        .unwrap();
    let calls = fake.calls();
    assert_eq!(calls[0].request, IoctlRequest::PIN_SETFUNC2);
    let raw: RawPinDesc = read_at(&calls[0].buffer);
    assert_eq!((raw.index, raw.func, raw.chan), (2, 1, 1));
    assert_eq!(raw.rsv, [0; 5]);
}

#[test]
fn pin_functions_fall_back_to_v1() {
    let (clock, fake) = clock_with(|request, buffer| match request {
        IoctlRequest::PIN_GETFUNC2 => Err(errno(libc::ENOTTY)),
        IoctlRequest::PIN_GETFUNC => {
            let mut raw: RawPinDesc = read_at(buffer);
            raw.func = 2;
            write_at(buffer, &raw);
            Ok(())
        }
        other => panic!("unexpected {other:?}"),
    });

    assert_eq!(clock.pin_function(0).unwrap().func, PinFunc::Perout);
    assert_eq!(fake.calls().len(), 2);
}

#[test]
fn pps_passes_the_boolean_as_a_scalar() {
    let (clock, fake) = clock_with(|_, _| Ok(()));
    clock.enable_pps(true).unwrap();
    clock.enable_pps(false).unwrap();
    let calls = fake.calls();
    assert_eq!(
        (calls[0].request, calls[0].value),
        (IoctlRequest::ENABLE_PPS2, Some(1))
    );
    assert_eq!(
        (calls[1].request, calls[1].value),
        (IoctlRequest::ENABLE_PPS2, Some(0))
    );
}

#[test]
fn pps_falls_back_to_the_v1_ioctl() {
    struct V1Only(Arc<Mutex<Vec<c_ulong>>>);

    impl Ioctl for V1Only {
        fn ioctl(
            &self,
            _: std::os::fd::BorrowedFd<'_>,
            _: IoctlRequest,
            _: &mut [u8],
        ) -> io::Result<()> {
            unreachable!("PPS uses the scalar path")
        }

        fn ioctl_value(
            &self,
            _: std::os::fd::BorrowedFd<'_>,
            request: IoctlRequest,
            value: c_ulong,
        ) -> io::Result<()> {
            match request {
                IoctlRequest::ENABLE_PPS2 => Err(errno(libc::ENOTTY)),
                IoctlRequest::ENABLE_PPS => {
                    self.0.lock().unwrap().push(value);
                    Ok(())
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    let values = Arc::new(Mutex::new(Vec::new()));
    let clock = PtpClock::from_fd(null_fd(), Box::new(V1Only(Arc::clone(&values))));
    clock.enable_pps(true).unwrap();
    assert_eq!(*values.lock().unwrap(), vec![1]);
}

// ---------------------------------------------------------------------------
// Network-device hardware timestamping
// ---------------------------------------------------------------------------

#[test]
fn hwtstamp_set_returns_the_filter_the_driver_actually_enabled() {
    let ioctl = Fake::new(|request, buffer| {
        assert_eq!(request, IoctlRequest::HWTSTAMP_SET);
        let ifreq: RawIfReq = read_at(buffer);
        assert_eq!(&ifreq.name[..4], b"eth0");
        assert!(!ifreq.data.is_null());
        // SAFETY: data points at the crate's live hwtstamp_config.
        let requested = unsafe { ifreq.data.cast::<RawHwTimestamp>().read_unaligned() };
        assert_eq!(
            requested,
            RawHwTimestamp {
                flags: 0,
                tx_type: 1,
                rx_filter: 6
            }
        );
        // The driver downgrades the filter and writes the effective value back.
        let effective = RawHwTimestamp {
            flags: 0,
            tx_type: 1,
            rx_filter: 12,
        };
        // SAFETY: as above.
        unsafe {
            ifreq
                .data
                .cast::<RawHwTimestamp>()
                .write_unaligned(effective)
        };
        Ok(())
    });
    let timestamping = HwTimestamping::with_ioctl(Box::new(Shared(Arc::clone(&ioctl))));
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();

    let effective = timestamping
        .set(&socket, "eth0", HwTxType::On, HwRxFilter::PtpV2L4Event)
        .unwrap();
    assert_eq!(effective, HwRxFilter::PtpV2Event);
    assert_eq!(ioctl.calls().len(), 1);
}

#[test]
fn hwtstamp_get_decodes_the_configuration() {
    let ioctl = Fake::new(|request, buffer| {
        assert_eq!(request, IoctlRequest::HWTSTAMP_GET);
        let ifreq: RawIfReq = read_at(buffer);
        assert_eq!(&ifreq.name[..3], b"lo\0");
        let config = RawHwTimestamp {
            flags: 1,
            tx_type: 2,
            rx_filter: 12,
        };
        // SAFETY: data points at the crate's live hwtstamp_config.
        unsafe { ifreq.data.cast::<RawHwTimestamp>().write_unaligned(config) };
        Ok(())
    });
    let timestamping = HwTimestamping::with_ioctl(Box::new(Shared(Arc::clone(&ioctl))));
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();

    let config = timestamping.get(&socket, "lo").unwrap();
    assert_eq!(
        config,
        HwTimestampConfig {
            flags: 1,
            tx_type: HwTxType::OneStepSync,
            rx_filter: HwRxFilter::PtpV2Event,
        }
    );
    assert!(config.uses_bonded_phc_index());
}

#[test]
fn hwtstamp_rejects_invalid_interface_names_without_calling_the_kernel() {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let ioctl = Fake::new(|_, _| Ok(()));
    let timestamping = HwTimestamping::with_ioctl(Box::new(Shared(Arc::clone(&ioctl))));

    for name in ["", "0123456789abcdef"] {
        let err = timestamping.get(&socket, name).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidArgument);
        assert_eq!(err.operation(), Operation::HwTimestampGet);
    }
    assert!(ioctl.calls().is_empty());
}

#[test]
fn hwtstamp_on_a_real_loopback_socket_reports_a_typed_error() {
    // Real ioctl, real socket, no PHC: the kernel either denies the capability
    // or the loopback device has no hardware timestamping operation.
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let err = HwTimestamping::new()
        .get(&socket, "lo")
        .expect_err("loopback has no hardware timestamping");
    assert!(
        matches!(
            err.kind(),
            ErrorKind::PermissionDenied | ErrorKind::NotSupported | ErrorKind::NotFound
        ),
        "unexpected kind {:?}",
        err.kind()
    );
    assert_eq!(err.operation(), Operation::HwTimestampGet);
    assert!(err.to_string().contains("SIOCGHWTSTAMP"));
}

// ---------------------------------------------------------------------------
// Time steering. The syscalls need a real PHC, so on a non-PHC fd only the
// typed error path is observable; the conversions are unit-tested.
// ---------------------------------------------------------------------------

#[test]
fn time_syscalls_on_a_non_phc_fd_name_the_failed_operation() {
    let (clock, _) = clock_with(|_, _| Ok(()));

    for (err, operation) in [
        (clock.get_time().unwrap_err(), Operation::GetTime),
        (
            clock.set_time(ClockTime::ZERO).unwrap_err(),
            Operation::SetTime,
        ),
        (
            clock.adjust_freq(0).unwrap_err(),
            Operation::AdjustFrequency,
        ),
        (clock.frequency_ppb().unwrap_err(), Operation::ReadFrequency),
        (clock.adjust_phase(0).unwrap_err(), Operation::AdjustPhase),
        (
            clock.step_time(ClockTime::ZERO).unwrap_err(),
            Operation::StepTime,
        ),
    ] {
        assert_eq!(err.operation(), operation);
        assert!(err.io_error().is_some());
    }
}

#[test]
fn out_of_range_arguments_never_reach_the_kernel() {
    let (clock, fake) = clock_with(|_, _| Ok(()));
    let err = clock.adjust_freq(i64::MAX).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidArgument);
    assert_eq!(err.operation(), Operation::AdjustFrequency);
    assert!(fake.calls().is_empty());
}

#[test]
fn errors_keep_their_source_chain() {
    let err = Error::Open {
        path: "/dev/ptp0".into(),
        source: errno(libc::ENOENT),
    };
    assert_eq!(err.kind(), ErrorKind::NotFound);
    let source = std::error::Error::source(&err).unwrap();
    assert!(source.to_string().contains("No such file"));
}
