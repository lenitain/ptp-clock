# ptp-clock — Linux PTP 硬件时钟（PHC）的完整机制层

## 背景：为什么存在这份文档（分层模型与术语）

> 本段是**自包含的上下文**，供首次阅读者（包括新会话中的 AI agent / 新协作者）理解本项目在整个 Rust 生态中的位置。后文的 "Layer 2 / 机制层 / 政策层 / 止步点" 等术语全部由本节定义，不需要任何外部对话背景。

### 问题背景：一类反复出现的生态缺口

Rust 生态中存在一类反复出现的缺口：很多领域已经有"能调用系统接口"的 crate，但缺少一个把该领域**协议 / 解析 / 正确性**做完整做正确的 crate——使用者被迫在自己的应用里反复实现同一份困难逻辑，而且很容易做错（结构体布局、跨架构位宽、字节序、能力门控、事件队列）。本 crate 就是为了填补这类缺口而设计。

### 四层模型

- **Layer 0 — 裸绑定**：`libc`、`libxcb` 等，直接暴露 syscall / C 接口。零安全、无错误语义。
- **Layer 1 — 薄安全封装**：`nix` 风格。把 syscall 包成安全函数（`Result`、RAII），但**没有解决领域问题**。
- **Layer 2 — 完整机制层（本 crate 的目标）**：把该领域的协议、解析、状态管理、类型化访问、错误语义做完整做正确。这是**正确性问题**。
- **Layer 3 — 政策 / 运行时 / 框架层**：事件循环、线程与异步模型、缓存策略、对象建模、业务决策。这是**偏好问题**。

### 核心原则（本 crate 的设计红线）

- **机制层必须做完整**：正确性问题不能甩给用户。
- **政策层必须留白**：不替用户决定偏好问题。"止步点"就是这两层的分界——再往前做一步，就是替用户做有分歧的决定，即"无端揣测约束用户行为"。
- **保留逃生舱**：提供 `as_fd()` / 原始访问等途径，让用户能接入自己的事件循环、缓存、策略。

### 参照系：fanotify-fid 之于 fanotify（本模式的原型）

`fanotify-fid`（Layer 2）把 fanotify 的协议层做完整：变长事件遍历、FID/DFID/DFID_NAME 记录解析、句柄回路径、可插拔缓存、RAII、带 man-page 级语义的错误类型。它刻意停在 Layer 2：不内置事件循环、不做 mount 自动发现、不强制缓存策略。本 crate 与 PTP 的关系同构：内核给的是"能力门控的 ioctl 家族 + 事件队列"，正确封装就是 Layer 2 的价值。

### 缺口分类（本 crate 的由来）

本 crate 属于 **B 类（机制锁在应用里）**：`rtime-clock`（crates.io）的 `PhcClock` 只服务其 NTP/PTP 守护进程（且只覆盖读/steer），`linuxptp` 是 C 工具；没有任何独立、完整的 Rust 机制库把 `/dev/ptpN` 的 ioctl 家族完整封装。

### 术语速查

- **机制层** = Layer 2 = 本 crate 的"范围 / 必须做"。
- **政策层** = Layer 3 = 本 crate 的"非目标 / 必须不做"。
- **止步点** = 机制层与政策层的分界。
- **Layer 1** = 现有的薄封装（可能不存在，也可能存在但不顶用）。

## 定位

**一句话**：把 `/dev/ptpN` 的 `PTP_*` ioctl 家族和硬件时间戳（`SIOCSHWTSTAMP`）封装成完整、类型化、RAII、带能力门控的机制层，供时间同步、测试测量、汽车/工业应用直接使用。**硬件相关的 ioctl 与事件队列读取是机制组成部分，完整实现，不做裁剪。**

**现状：为何缺少 Layer 2（机制层）**：

| 层 | 现状 | 问题 |
|---|---|---|
| Layer 1 | 裸 `ioctl`（`PTP_CLOCK_*`、`PTP_EXTTS_*`、`PTP_PEROUT_*`、`PTP_PIN_*`、`PTP_SYS_OFFSET_*`） | 每个 ioctl 都要手写 `repr(C)` 结构体、能力检查、错误处理；跨架构结构体布局要小心；V1/V2 ioctl 选择与降级全凭经验 |
| Layer 2 | **机制被锁在应用里** | `rtime-clock` 的 `PhcClock` 只服务其守护进程（只覆盖读/steer）；`linuxptp` 是 C 工具。没有任何独立、完整的 Rust 机制库 |
| Layer 3 | 时间守护进程 | 时钟伺服算法、PTP 协议状态机、NTP 互操作——应用的事 |

## 范围（机制层 · 必须做）

1. **时钟能力查询**：`PTP_CLOCK_GETCAPS` / `PTP_CLOCK_GETCAPS2` → 类型化的 `Capabilities`（max_adj、n_ext_ts、n_per_out、pps、n_pins、cross_timestamping、adjust_phase、max_phase_adj），据此暴露可用 API。
2. **时间读写**：PHC 是 POSIX 动态时钟（`clock_posix_dynamic`），**没有** GETTIME/SETTIME ioctl——读写与调频走 `clock_gettime` / `clock_settime` / `clock_adjtime`，并把 **fd 当 `clockid_t`**（见"内核事实 F2"）。
3. **系统偏移**：`PTP_SYS_OFFSET`（`PTP_SYS_OFFSET2`）、`PTP_SYS_OFFSET_PRECISE`（`..._PRECISE2`）、`PTP_SYS_OFFSET_EXTENDED`（`..._EXTENDED2`）→ 成对的系统/PHC 采样结果类型化。
4. **外部时间戳**：`PTP_EXTTS_REQUEST` / `PTP_EXTTS_REQUEST2`——配置、订阅、从 fd `read()` 事件队列读取时间戳事件；RAII 句柄（drop 时释放）。
5. **周期输出**：`PTP_PEROUT_REQUEST` / `PTP_PEROUT_REQUEST2`——周期信号输出配置；RAII 句柄。
6. **引脚功能**：`PTP_PIN_GETFUNC` / `PTP_PIN_SETFUNC`（含 `...2`）——引脚绑定（EXTTS/PEROUT/PHYSYNC）。
7. **PPS**：`PTP_ENABLE_PPS`（含 `...2`）。
8. **硬件时间戳（网卡侧）**：`SIOCSHWTSTAMP` / `SIOCGHWTSTAMP`——socket 时间戳使能；与 PHC 配合给出完整的时间戳获取路径。
9. **能力门控**：按 `Capabilities` 暴露可用 API（而不是让用户逐个试），能力查询失败降级。
10. **RAII**：`/dev/ptpN` fd、外部时间戳/周期输出句柄；错误带 man-page 级描述。

## 非目标（政策层 · 必须不做）

- **不**实现 PTP 协议状态机（`PTP_*` 报文、Sync/Follow_Up/Delay_Req——那是 linuxptp 的活）。
- **不**做时钟伺服/驯服算法（PI 控制器、锁相环——那是守护进程的活）。
- **不**与 NTP 互操作。
- **不**替你选"用哪个 PHC"（`/dev/ptpN` 的发现/打分是上层策略——本 crate 提供 `open(index)` 与 `open_path`，不做自动发现）。
- **不**绑定异步：同步 API + `AsFd`，用户接入自己的循环。
- **不**做 `SO_TIMESTAMPING` 报文时间戳的完整接收路径（错误队列/`recvmsg` 解析是网络栈机制，超出 PHC 机制层范围；本 crate 只做 `SIOCSHWTSTAMP` 配置侧 + `PTP_EXTTS` 事件侧）。

## 止步点：最大化替上层做事，同时不约束上层

这条线不按"做多做少"划，而按**问题类型**划：

- **必须做（机制层）**：正确性问题 —— 协议编解码、结构体布局、RAII、错误语义、能力门控、容错降级。不做，上层每个使用者都要重复实现同一份困难逻辑，且容易做错。
- **必须不做（政策层）**：偏好问题 —— 事件循环/异步运行时、线程模型、缓存/记忆策略、资源发现策略、时钟选择策略、业务语义约定。没有唯一正确答案，做了就是在替上层做决定，导致"因为这个库我不得不这么写"。
- **留逃生舱**：`AsFd` / 原始字节 / 裸函数入口，机制不满足时上层可绕过，不必改库。
- **可插拔**：用 trait / 回调让上层注入策略，不写死唯一默认方案。
- **破坏性操作显式化**：清除/删除/修改绝不隐式发生，只有显式调用才执行。
- **数据不因"不识别"而丢失**：解析失败降级保留（原始数据 + Unknown 标记），而非报错丢弃。

**反面自检**（写每个 API 前问自己）：

1. 这个 API 是不是在替上层决定"它怎么跑"（循环/线程/时机）？→ 是则不做。
2. 是否隐含了只适用部分场景的假设（"总是自动发现""总是缓存"）？→ 是则做成可选/可插拔。
3. 上层是否必须付出代价才能绕过我（改源码 / 开 unsafe / 重新解析我吞掉的信息）？→ 是则留逃生舱。
4. 破坏性行为是否可能在用户不知情时发生？→ 是则改为显式。

**本项目具体的止步点**：

- 时钟伺服/驯服算法、PTP 协议状态机、NTP 互操作、选哪个 PHC、`SO_TIMESTAMPING` 完整接收路径 → 政策/分工，不做。
- `PTP_*` ioctl 族类型化封装 + 能力门控 + 外部时间戳事件队列 + `SIOCSHWTSTAMP` 配置 → 机制，必须做。
- 逃生舱：`as_fd()`；能力查询失败降级（而非整体失败）；原始 ioctl 透传入口。

**正面参照（linux-pstore 已实现）**：

- 数据永远给原始字节（`Record.data`），压缩记录同时给 `is_compressed()` 与 `decompressed()`，上层可以选择不解压。
- 解析失败的文件**仍被枚举和读取**（Unknown 类型），绝不丢数据。
- `clear()` 是唯一清除入口，完全显式，无任何"读完即清"的隐式行为。
- 系统相关探测抽成可注入函数（`open_inner`），既让系统分支（NotMounted）可单测，也让 API 保持单一职责。→ 本项目扩展到 ioctl：**ioctl 分发抽成可注入边界**，让硬件路径的成功分支也能在模拟中测试，见"设计决策"。

## 命名

`ptp-clock`：`ptp` 和 `phc` 都已被占用；`ptp-clock` 是 PTP 硬件时钟的规范叫法（man `ptp_clock(2)`），直白可检索。

## API 草案

```rust
// ioctl 注入边界：默认 impl 直接调 libc::ioctl；测试注入可模拟内核回写与 errno。
pub trait Ioctl {
    fn ioctl(&self, fd: BorrowedFd<'_>, request: IoctlRequest, buffer: &mut [u8]) -> io::Result<()>;
    fn ioctl_value(&self, fd: BorrowedFd<'_>, request: IoctlRequest, value: c_ulong) -> io::Result<()>;
}
pub struct SystemIoctl;
pub struct IoctlRequest(/* c_ulong */);       // PTP_* 与 SIOC[GS]HWTSTAMP 常量 + raw()/name()

pub struct PtpClock { /* OwnedFd + Box<dyn Ioctl + Send + Sync> + writable */ }
pub struct Capabilities { /* max_adj, n_alarm, n_ext_ts, n_per_out, pps, n_pins, cross_timestamping, adjust_phase, max_phase_adj */ }
pub struct ClockTime { /* sec: i64, nsec: u32 —— 对应 struct ptp_clock_time，构造时归一化 */ }
pub struct ExtTsFlags(u32);                   // ENABLE_FEATURE / RISING_EDGE / FALLING_EDGE / EXT_OFFSET
pub struct ExtTsRequest { pub index: u32, pub flags: ExtTsFlags }
pub enum ExtTsEdge { Rising, Falling, Both }
pub enum PeroutStart { Absolute(ClockTime), Phase(ClockTime) }
pub struct PeroutRequest { pub index, pub period, pub start: PeroutStart, pub duty_cycle: Option<ClockTime>, pub one_shot: bool }
pub enum PinFunc { None, ExtTs, Perout, Physync, Unknown(u32) }
pub struct PinFunction { pub index: u32, pub func: PinFunc, pub chan: u32 }
pub struct PinDescription { pub name: Option<String>, pub index: u32, pub func: PinFunc, pub chan: u32 }
pub struct SysOffset { pub samples: Vec<SysOffsetSample>, pub sys_after: ClockTime }
pub struct SysOffsetSample { pub sys: ClockTime, pub phc: ClockTime }
pub struct SysOffsetPrecise { pub device: ClockTime, pub sys_realtime: ClockTime, pub sys_monoraw: ClockTime }
pub struct SysOffsetExtended { pub samples: Vec<SysOffsetExtendedSample> }
pub struct SysOffsetExtendedSample { pub pre: ClockTime, pub phc: ClockTime, pub post: ClockTime }
pub struct ExtTsEvent { /* time, index, flags —— 与 struct ptp_extts_event 同布局，fd 直接 read */ }

impl PtpClock {
    pub fn open(index: u32) -> Result<Self>;              // /dev/ptp{index}，O_RDWR→O_RDONLY 降级
    pub fn open_path(path: impl AsRef<Path>) -> Result<Self>;  // 逃生舱
    pub fn from_fd(fd: OwnedFd, ioctl: Box<dyn Ioctl + Send + Sync>) -> Self; // 注入/虚拟设备
    pub fn as_fd(&self) -> BorrowedFd<'_>;                // 逃生舱：事件队列直接 read/poll
    pub fn is_writable(&self) -> bool;
    pub fn capabilities(&self) -> Result<Capabilities>;   // GETCAPS2，ENOTTY 降级 GETCAPS
    pub fn get_time(&self) -> Result<ClockTime>;          // clock_gettime(fd)
    pub fn set_time(&self, t: ClockTime) -> Result<()>;   // clock_settime(fd)，需 O_RDWR
    pub fn adjust_freq(&self, ppb: i64) -> Result<()>;    // ADJ_FREQUENCY，ppb→scaled ppm 精确反解
    pub fn frequency_ppb(&self) -> Result<i64>;           // clock_adjtime(modes = 0)
    pub fn adjust_phase(&self, nanos: i64) -> Result<()>; // ADJ_OFFSET | ADJ_NANO
    pub fn step_time(&self, offset: ClockTime) -> Result<()>; // ADJ_SETOFFSET | ADJ_NANO
    pub fn sys_offset(&self, n_samples: u32) -> Result<SysOffset>;
    pub fn sys_offset_precise(&self) -> Result<SysOffsetPrecise>;
    pub fn sys_offset_extended(&self, n_samples: u32, clockid: clockid_t) -> Result<SysOffsetExtended>;
    pub fn request_ext_ts(&self, req: &ExtTsRequest) -> Result<ExtTsHandle<'_>>;   // RAII
    pub fn request_perout(&self, req: &PeroutRequest) -> Result<PeroutHandle<'_>>; // RAII
    pub fn read_event(&self) -> Result<Option<ExtTsEvent>>;        // EAGAIN → Ok(None)
    pub fn read_events(&self, events: &mut [ExtTsEvent]) -> Result<usize>; // 零拷贝，支持部分读
    pub fn pin_function(&self, index: u32) -> Result<PinDescription>;
    pub fn set_pin_function(&self, f: &PinFunction) -> Result<()>;
    pub fn enable_pps(&self, enabled: bool) -> Result<()>;
}
pub struct ExtTsHandle<'a> { /* drop 写 enable=0；release() 返回错误，disarm() 显式放弃 */ }
pub struct PeroutHandle<'a> { /* drop 写 period=0；release()/disarm() */ }

// 错误：operation（哪个 API/ioctl）+ kind（ENOTTY/EINVAL/EBUSY/EPERM 归类）+ source(errno)
pub enum Error { Open { .. }, Ioctl { .. }, Read { .. }, InvalidRequest { .. } }
pub enum ErrorKind { NotSupported, InvalidArgument, Busy, PermissionDenied, NotFound, Other }
pub enum Operation { /* 每个公开 API 一个变体 */ }

// 网卡侧硬件时间戳配置：持有注入边界（与 PtpClock 同构），纯转换函数可单测
pub enum HwTxType { Off, On, OneStepSync, OneStepP2P, Unknown(i32) }
pub enum HwRxFilter { None, All, Some, PtpV1L4Event, /* …全集… */ NtpAll, Unknown(i32) }
pub struct HwTimestampConfig { pub flags: u32, pub tx_type: HwTxType, pub rx_filter: HwRxFilter }
pub struct HwTimestamping { /* Box<dyn Ioctl + Send + Sync> */ }
impl HwTimestamping {
    pub fn new() -> Self;
    pub fn with_ioctl(ioctl: Box<dyn Ioctl + Send + Sync>) -> Self;
    pub fn set<F: AsFd>(&self, sock: F, ifname: &str, tx: HwTxType, rx: HwRxFilter) -> Result<HwRxFilter>;
    pub fn get<F: AsFd>(&self, sock: F, ifname: &str) -> Result<HwTimestampConfig>;
}

// 模块划分
//   src/clock.rs     PHC 打开 + 能力 + 时间
//   src/offset.rs    sys_offset 系列
//   src/extts.rs     外部时间戳 + 事件队列 + RAII 句柄
//   src/perout.rs    周期输出 + RAII 句柄
//   src/pin.rs       引脚功能
//   src/pps.rs       PPS 使能
//   src/socktstamp.rs SIOCSHWTSTAMP 配置
//   src/ioctl.rs      ioctl 注入边界（测试）
//   src/error.rs     类型化错误
```

## 内核事实（已核实 · 2026-08-04，实现前无需重查）

> 全部逐条对照本机 `/usr/include/linux/ptp_clock.h`、`net_tstamp.h`、`sockios.h`（与内核 7.1.5 一致）逐字核实。出处列在每行。

### F1. 设备与 ioctl 家族

| 事实 | 出处 |
|---|---|
| PHC 是字符设备 `/dev/ptpN`（N 为索引）；`open()` 后 `ioctl()` 操作 | `ptp_clock.h` + man `ptp_clock(2)` |
| ioctl 魔术字节 `PTP_CLK_MAGIC '='`（0x3D）；编号从 1 开始 | `ptp_clock.h` |
| **V1 ioctl**（无严格标志校验）：`PTP_CLOCK_GETCAPS`(1)、`PTP_EXTTS_REQUEST`(2)、`PTP_PEROUT_REQUEST`(3)、`PTP_ENABLE_PPS`(4)、`PTP_SYS_OFFSET`(5)、`PTP_PIN_GETFUNC`(6)、`PTP_PIN_SETFUNC`(7)、`PTP_SYS_OFFSET_PRECISE`(8)、`PTP_SYS_OFFSET_EXTENDED`(9) | `ptp_clock.h` |
| **V2 ioctl**（严格标志校验，编号 +9）：`..._GETCAPS2`(10)、`..._EXTTS_REQUEST2`(11)、`..._PEROUT_REQUEST2`(12)、`..._ENABLE_PPS2`(13)、`..._SYS_OFFSET2`(14)、`..._PIN_GETFUNC2`(15)、`..._PIN_SETFUNC2`(16)、`..._SYS_OFFSET_PRECISE2`(17)、`..._SYS_OFFSET_EXTENDED2`(18) | `ptp_clock.h` |
| 更新（v6.x+）：`PTP_MASK_CLEAR_ALL`(19)、`PTP_MASK_EN_SINGLE`(20)、`PTP_SYS_OFFSET_PRECISE_CYCLES`(21)、`PTP_SYS_OFFSET_EXTENDED_CYCLES`(22)——v1 可不实现，常量保留 | `ptp_clock.h` |
| **策略**：优先用 V2（严格标志校验），内核不支持 V2 时报 `ENOTTY` 再降级 V1；`PTP_EXTTS_REQUEST2` 下 `PTP_STRICT_FLAGS` 由内核始终强制 | `ptp_clock.h` 注释 |

### F2. 时间读写（已核实 · 2026-11-14，内核 7.4.0-rc2 源码）

| 事实 | 出处 |
|---|---|
| `ptp_clock.h` **没有单独的 GETTIME/SETTIME ioctl**；PHC 通过 `posix_clock_register()` 注册为 POSIX 动态时钟（`clock_posix_dynamic`），时间操作以 **fd 当 `clockid_t`** 分发 | `drivers/ptp/ptp_clock.c`（`ptp_clock_ops`）、`kernel/time/posix-clock.c` |
| `get_time` = `clock_gettime(fd_to_clockid(fd), &ts)` → `pc_clock_gettime` → `ptp_clock_gettime`（优先 `gettimex64`，否则 `gettime64`）；注册时强制二者至少其一，故核心层不会 `EOPNOTSUPP` | `posix-clock.c:248`、`ptp_clock.c:111`、`ptp_clock.c:336` |
| `set_time` = `clock_settime(fd_to_clockid(fd), &ts)` → `pc_clock_settime`：fd 必须 **O_RDWR**（`FMODE_WRITE`，否则 `EACCES`）；`timespec64_valid_settod` 失败 `EINVAL`；PHC 被虚拟时钟占用时 `EBUSY`；注册时强制 `settime64` 存在。**核心层不检查 `CAP_SYS_TIME`**（与 `PTP_ENABLE_PPS` 不同），权限由设备节点 + O_RDWR 决定 | `posix-clock.c:286`、`ptp_clock.c:96` |
| `adjust_freq` = `clock_adjtime(fd_to_clockid(fd), &timex)`，`modes = ADJ_FREQUENCY`：`tx.freq` 是 scaled ppm（16 位二进制小数），内核算 `ppb = (1 + ppm) * 125 >> 13`，`\|ppb\| > max_adj` → `ERANGE`，否则调 `info->adjfine(ops, tx.freq)`。modes≠0 时同样要求 O_RDWR | `posix-clock.c:224`、`ptp_clock.c:123–180`、`include/linux/ptp_clock_kernel.h:264` |
| 反向换算（ppb → scaled ppm）必须取 `ppm = ceil(ppb * 8192 / 125) - 1`，才能保证内核算回的 ppb 与请求值完全相等（否则 `max_adj` 边界会差 1 ppb） | 由上式反解 + 单测 |
| `clock_adjtime(modes = 0)` 回读 `tx.freq = dialed_frequency`（scaled ppm），可用于 `frequency_ppb()` | `ptp_clock.c:196` |
| `modes = ADJ_OFFSET \| ADJ_NANO` → `info->adjphase(offset_ns)`（无 `adjphase` 则 `EOPNOTSUPP`，超出 `max_phase_adj` 则 `ERANGE`）；`modes = ADJ_SETOFFSET \| ADJ_NANO` → `info->adjtime(delta)`（时间步进） | `ptp_clock.c:180–195` |
| `clock_getres(fd)` 恒返回 1 ns（`ptp_clock_getres`），无实用价值，不暴露 | `ptp_clock.c:89` |
| clockid 编码：`fd_to_clockid(fd) = ((~(unsigned)fd) << 3) \| 3`，反向 `clockid_to_fd(clk) = ~(clk >> 3)`；`CLOCKFD = 3`。用户态 libc 不提供该宏，需自行计算 | `include/linux/posix-timers.h:33`、`include/linux/posix-timers_types.h:31` |

### F3. 能力与结构体（逐字核实）

| 事实 | 出处 |
|---|---|
| `struct ptp_clock_time { s64 sec; u32 nsec; u32 reserved; }`（16 字节）；**sec 的符号作用于整个值，nsec 恒为非负** | `ptp_clock.h` |
| `struct ptp_clock_caps { int max_adj, n_alarm, n_ext_ts, n_per_out, pps, n_pins, cross_timestamping, adjust_phase, max_phase_adj; int rsv[11]; }` | `ptp_clock.h` |
| `struct ptp_extts_request { u32 index, flags, rsv[2]; }`；flags 位：`PTP_ENABLE_FEATURE(1<<0)`、`PTP_RISING_EDGE(1<<1)`、`PTP_FALLING_EDGE(1<<2)`、`PTP_STRICT_FLAGS(1<<3)`、`PTP_EXT_OFFSET(1<<4)` | `ptp_clock.h` |
| `PTP_EXTTS_REQUEST` 只接受 `PTP_EXTTS_V1_VALID_FLAGS`（ENABLE/RISING/FALLING）；`REQUEST2` 接受 `PTP_EXTTS_VALID_FLAGS`（+STRICT/EXT_OFFSET） | `ptp_clock.h` |
| `struct ptp_perout_request { union{start,phase}; period; u32 index, flags; union{on, rsv[4]}; }`；flags：`PTP_PEROUT_ONE_SHOT(1<<0)`、`DUTY_CYCLE(1<<1)`、`PHASE(1<<2)`；**period 为 0 表示禁用**；`PHASE` 置位时用 `phase` 字段，否则用 `start` | `ptp_clock.h` |
| `PTP_MAX_SAMPLES = 25`；`struct ptp_sys_offset { u32 n_samples, rsv[3]; struct ptp_clock_time ts[2*25+1]; }`——**结果为 2*n_samples+1 个交错时间戳，最后一个是系统时间戳**（数组布局 [sys, phc, sys, phc, ..., sys]） | `ptp_clock.h` 注释 |
| `struct ptp_sys_offset_extended { u32 n_samples; clockid_t clockid; u32 rsv[2]; struct ptp_clock_time ts[25][3]; }`——每样本 [pre-TS, PHC, post-TS]；**clockid 字段从 6.12 起使用**（此前 rsv[3]，向后兼容：CLOCK_REALTIME=0） | `ptp_clock.h` 注释 |
| `struct ptp_sys_offset_precise { device, sys_realtime, sys_monoraw; u32 rsv[4]; }` | `ptp_clock.h` |
| `enum ptp_pin_function { PTP_PF_NONE, PTP_PF_EXTTS, PTP_PF_PEROUT, PTP_PF_PHYSYNC }`；`struct ptp_pin_desc { char name[64]; u32 index, func, chan; u32 rsv[5]; }`——name 由内核在 GETFUNC 时填写 | `ptp_clock.h` |
| `struct ptp_extts_event { struct ptp_clock_time t; u32 index, flags, rsv[2]; }`（32 字节）——从事件 fd `read()` 得到的是**该结构的数组** | `ptp_clock.h` |

### F4. 硬件时间戳（网卡侧）

| 事实 | 出处 |
|---|---|
| `SIOCSHWTSTAMP = 0x89b0`、`SIOCGHWTSTAMP = 0x89b1`；通过 `struct ifreq` 的 `ifr_data` 指向 `struct hwtstamp_config` | `sockios.h` + `net_tstamp.h` |
| `struct hwtstamp_config { int flags, tx_type, rx_filter; }`；**SIOCSHWTSTAMP 若驱动不支持请求的 rx_filter 可能降级为更通用的模式，返回的 rx_filter 是实际生效值** | `net_tstamp.h` |
| `enum hwtstamp_tx_types`：`HWTSTAMP_TX_OFF`、`TX_ON`、`TX_ONESTEP_SYNC`、`TX_ONESTEP_P2P` | `net_tstamp.h` |
| `enum hwtstamp_rx_filters`：`NONE`、`ALL`、`SOME`、`PTP_V1_L4_EVENT/SYNC/DELAY_REQ`、`PTP_V2_L4_EVENT/SYNC/DELAY_REQ`、`PTP_V2_L2_EVENT/SYNC/DELAY_REQ`、`PTP_V2_EVENT/SYNC/DELAY_REQ`、`NTP_ALL` | `net_tstamp.h` |
| `enum hwtstamp_flags`：`HWTSTAMP_FLAG_BONDED_PHC_INDEX`（bond 活跃接口的 PHC 索引） | `net_tstamp.h` |
| `SO_TIMESTAMPING` 标志（`SOF_TIMESTAMPING_*`）用于报文时间戳报告——**本 crate 不做接收解析**（非目标） | `net_tstamp.h` |

### F5. 生态现状（Layer 1 边界核实）

| 事实 | 出处 |
|---|---|
| `rtime-clock`（crates.io）的 `PhcClock` 只覆盖读/steer（`clock_adjtime`），服务其守护进程，非独立完整机制库 | crates.io 调查 |
| `linuxptp`（ptp4l/phc2sys）是 C 工具，其 PHC 操作逻辑未抽成可复用库 | 项目事实 |

## 设计决策（已定，不要推翻重来）

1. **能力门控 + 降级**：先 `GETCAPS2`，失败（`ENOTTY`/`EOPNOTSUPP`）降级 `GETCAPS`；能力查询失败不整体失败——`Capabilities` 保留原始字段，未确认的能力按"不支持"处理（而非报错），用户可依赖 `as_fd` 自救。
2. **V2 优先、V1 降级**：所有 ioctl 优先 V2；内核不支持（`ENOTTY`）时降级 V1；`ExtTsRequest` 构造时按目标 ioctl 校验合法标志（V1 不接受 STRICT/EXT_OFFSET）。
3. **ioctl 注入边界（不回避硬件）**：ioctl 分发抽成可注入边界（默认 `libc::ioctl`，测试注入假实现模拟内核返回/错误）。请求结构体构造、响应结构体解析、`sys_offset` 交错数组解释、`extts_event` 数组解析全部纯函数——**成功路径与错误路径都在本机测**，只留最终 syscall 命中给真机。真实用法给 `no_run` doctest + 可选 example。
4. **RAII 句柄**：`ExtTsHandle`/`PeroutHandle` 在 drop 时显式释放（写 enable=0 / period=0）；释放失败记日志不 panic；`disarm()` 显式放弃释放。**绝不**在 drop 里做 set_time 等破坏性操作。
5. **结构体布局**：全部 `#[repr(C)]`；`sec` 用 `i64`、`nsec` 用 `u32`；大小用 `size_of::<T>()` 单测断言（对齐内核 UAPI，防跨架构漂移）。
6. **破坏性操作显式**：`set_time`、`adjust_freq`、`enable_pps`、`set_pin_func` 都是显式调用；无任何隐式改动。
7. **零依赖核心**：ioctl 封装只用 `libc`；`default-features = false` 必须可构建。

## 正确性要点（实现时逐条验证）

- `ptp_sys_offset` 交错数组：**2*n_samples+1** 项，偶数位（0,2,4,…）是系统时间戳、奇数位是 PHC 时间戳、**最后一项是系统时间戳**（不是 PHC）——解析不能错位。
- `sys_offset_extended`：每样本三值 [pre, PHC, post]，n_samples 行；`clockid` 字段 6.12+，向旧内核发送时置 0。
- `extts_event` 数组读取：一次 `read()` 可能返回 0..N 个事件；`EAGAIN`（非阻塞）→ `Ok(None)`；按 32 字节对齐处理部分读。
- 结构体大小断言：`ptp_clock_time`=16、`ptp_clock_caps`=80、`ptp_extts_request`=16（4+4+4*2）、`ptp_perout_request`=56、`ptp_sys_offset`=832（4+12+16*51）、`ptp_sys_offset_extended`=1216（16+16*75）、`ptp_sys_offset_precise`=64、`ptp_extts_event`=32（16+4+4+8）、`ptp_pin_desc`=96（64+12+20）（用 `size_of` 断言，防平台漂移）。`ifreq` 按内核尺寸留足（64 位 40 字节、32 位 32 字节），并按 `ifr_name`/`ifr_data` 偏移（0/16）访问。
- 能力字段语义：`max_adj` 单位 ppb；`n_pins` 为 0 表示无引脚功能；`cross_timestamping`/`adjust_phase` 布尔。
- ioctl 错误透传：`ENOTTY`（不是 PHC/不支持 V2）、`EINVAL`（非法参数/标志）、`EBUSY`、`EPERM`——全部类型化并带上下文。
- `SIOCSHWTSTAMP`：通过 `ifreq` 设置 `ifr_name` + `ifr_data`；**返回的 rx_filter 可能被驱动降级**，必须回读并透传给调用者。

## 实现交付清单（一次性完成）

> 本清单来自 linux-pstore 的完整实现与事后审查：实现时**一次性**完成以下各项，避免完成后再返工。

### A. 内核事实核实（实现前 · 最优先）

- [x] 已核实 F1/F3/F4/F5（本机 `ptp_clock.h`/`net_tstamp.h`/`sockios.h` 逐字核对）。
- [x] **F2 时间读写路径**（2026-11-14，内核 7.4.0-rc2 源码）：PHC 是 POSIX 动态时钟，`get/set/adjust` 全走 fd-as-clockid 的 `clock_gettime`/`clock_settime`/`clock_adjtime`；写入需 O_RDWR；`ADJ_FREQUENCY` 用 scaled ppm。
- [x] `PTP_MASK_*`(19/20) 与 `_CYCLES`(21/22) 排除在 v1 范围外：前者是 per-open 事件队列掩码（6.14+），后者是 cycles 时间源（需 `has_cycles`），均非 PHC 基本机制；常量保留在 `IoctlRequest`。
- [x] 本机无 `/dev/ptp*`（2026-11-14 复核），全部走 ioctl 注入。

### B. API 文档（随实现一起写）

- [x] 每个公开项（类型/变体/字段/方法/常量）都有文档注释；`#![warn(missing_docs)]` 常驻，CI 以 `RUSTFLAGS=-D warnings` 强制。
- [x] 每个公开 API 有 doctest（纯逻辑可运行；真实硬件用法 `no_run` 编译校验），`cargo test` 中编译运行；`examples/inspect.rs` 给出真实设备用法。
- [x] README 只写总览 + 一个主示例；用法细节由各 API 的 doctest 承载。
- [x] 错误类型每个变体的字段都有文档；`Display` / `Error::source` 链完整。

### C. 测试覆盖

- [x] 单元测试覆盖每条解析分支（含未知标志/未知功能码保留）。
- [x] 集成测试覆盖公开 API 的 happy path + **每个错误变体**的触发路径（ENOTTY / EINVAL / ERANGE / EBUSY / EPERM / 文件不存在）。
- [x] 系统相关分支可测：ioctl 注入边界测成功与错误路径；`sys_offset` 交错数组解释、`extts_event` 解析用合成字节；`SIOC[GS]HWTSTAMP` 在真实 loopback socket 上测错误路径。
- [x] 特性矩阵：`cargo test`（默认）与 `cargo test --no-default-features` 都绿（crate 无 feature）。
- [x] 本项目测试重点：结构体大小断言；V2→V1 降级路径；`sys_offset` 数组 2n+1 布局；`extts_event` 读取/EAGAIN；RAII 句柄释放（drop 写 enable=0 / period=0）。

### D. 仓库卫生（对齐 fanotify-fid 约定）

- [x] `Cargo.lock` 不提交（库 crate），已加入 `.gitignore`（含 `/target`、编辑器杂项）。
- [x] DESIGN.md 是过程文件：README 不引用、Cargo.toml 的 `include` 不含 DESIGN.md。
- [x] LICENSE 文本对齐同系列：MIT 全文 + `Copyright (c) 2026 ptp-clock contributors`。
- [x] CI（`.github/workflows/ci.yml`）：test（特性矩阵）/ fmt / clippy / doc 四 job，`RUSTFLAGS="-D warnings"`。
- [x] 本地全绿：`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`。

### E. 特性设计

- [x] 无可选依赖；`default-features = false` 构建等价于默认构建（核心只用 `libc`）。
- [x] 特性矩阵进 CI（默认 + 无默认）。

## 参考

- 本机 UAPI 头：`/usr/include/linux/ptp_clock.h`、`net_tstamp.h`、`sockios.h`（内核 7.1.5，逐字核实）。
- man `ptp_clock(2)`、`clock_gettime(2)`、`clock_adjtime(2)`、`socket(7)`（`SO_TIMESTAMPING`）。
- 生态：`rtime-clock`（Layer 1 参照）、`linuxptp`（C 工具参照）。
- 对标：`fanotify-fid` 的"与既有库分工、不重复造轮子、政策留白"；`linux-iio` 的"ioctl 注入边界不回避硬件"。

## 实现前核实结论（已完成 · 2026-08-04）

> 本节是本项目开工前的调查结论，供新会话直接使用，避免重新调查。

### 环境约束（本机验证）

- 本机无 `/dev/ptp*`、无 PHC 硬件、无 root、无 sudo。
- **结论**：实现与测试**不能依赖真实 PHC**。全部走可注入路径：ioctl 注入边界模拟成功/错误、结构体构造/解析纯测、`sys_offset` 数组解释合成数据、`extts_event` 合成字节、`SIOCSHWTSTAMP` 用真实 loopback socket 测错误路径。真实设备验证留给有硬件的用户（`no_run` doctest + example）。
- 本机有 `/usr/include/linux/ptp_clock.h`、`net_tstamp.h`、`sockios.h`（内核 7.1.5）——**ioctl 编号、结构体布局、标志位已逐字核实**。

### 已核实结论速查

- ioctl 家族：V1(1–9) + V2(10–18) + 新(19–22)；魔术字符 `'='`；V2 优先、`ENOTTY` 降级 V1。
- 结构体：`ptp_clock_time`(16B)、`ptp_extts_request`(12B)、`ptp_perout_request`、`ptp_sys_offset`（2n+1 交错，末尾系统时间戳）、`ptp_sys_offset_extended`（[pre,PHC,post]，clockid 6.12+）、`ptp_sys_offset_precise`、`ptp_pin_desc`、`ptp_extts_event`(32B)。
- 标志：EXTTS（ENABLE/RISING/FALLING/STRICT/EXT_OFFSET）、PEROUT（ONE_SHOT/DUTY_CYCLE/PHASE，period=0 禁用）。
- 网卡侧：`SIOCSHWTSTAMP`=0x89b0 / `SIOCGHWTSTAMP`=0x89b1，`ifreq.ifr_data`→`hwtstamp_config`；rx_filter 可能被驱动降级。
- **F2 已核实**：PHC = POSIX 动态时钟，`clock_gettime` / `clock_settime` / `clock_adjtime`（fd 当 clockid）；`set/adj` 需 O_RDWR；`adj_freq` 用 scaled ppm，`ppb = ((1+ppm)*125)>>13`。
- 生态：`rtime-clock` 只覆盖读/steer → B 类。

### 已定的 API 决策（不要推翻）

- 见"设计决策"1–7：能力门控+降级；V2 优先 V1 降级；ioctl 注入边界；RAII 句柄+disarm；`repr(C)`+大小断言；破坏性显式；零依赖核心。
- 模块划分：clock / offset / extts / perout / pin / pps / socktstamp / ioctl / error。

### 开工第一天必须做的（其余见清单 A）

1. 通读本 DESIGN.md 全文 + 同目录 `AGENTS.md`。
2. 从 `src/ioctl.rs`（注入边界）+ 纯结构体编码起步，再 clock/offset → extts/perout（RAII）→ pin/pps → socktstamp。
3. 全部绿后一次性写入正式目录并单次提交。
