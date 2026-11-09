# AGENTS.md — ptp-clock 实现指南

> 本文件是给（重新）开始实现 ptp-clock 的 AI agent 的操作指南，来自同系列已完成的 `linux-pstore` 与 `apparmor-policy` 的实现复盘，并针对本项目的领域特化。它与 `DESIGN.md` 配合：DESIGN.md 讲"做什么、为什么"（背景/范围/止步点/已核实的内核事实 F1–F5/设计决策/交付清单），本文件讲"怎么做、别踩什么坑、本项目怎么验证"。**新会话必须先通读 DESIGN.md 全文（重点：背景、内核事实 F1–F5、设计决策 1–7、实现前核实结论），再读本文件。**

## 1. 项目定位（一句话）

本系列 crate 都是 **Layer 2 机制层**：把某个 Linux 子系统/协议的正确性问题一次做对（结构体布局、编解码、RAII、错误语义、能力门控、容错降级），把偏好问题（事件循环、线程、策略、缓存）留给上层。ptp-clock 就是 Linux PHC（`/dev/ptpN`）的完整机制层：能力查询、时间读写、系统偏移、外部时间戳/周期输出（RAII 句柄）、引脚功能、PPS、网卡侧 `SIOCSHWTSTAMP` 配置。核心红线：**机制层做完整（正确性问题不能甩给用户），政策层留白（不替用户做偏好决定）**。

## 2. 实现流程（按顺序，不要跳步）

1. **核实事实（最优先）**——DESIGN.md 的"内核事实"F1/F3/F4/F5 已核实并带出处（本机 `ptp_clock.h`/`net_tstamp.h`/`sockios.h` 逐字核对），**直接用，不要重新调查**；清单 A 剩 1 个大项（F2 时间读写路径：`get_time`/`set_time`/`adjust_freq` 走 `clock_adjtime` 还是 `clock_gettime(fd)`，对照 `drivers/ptp/ptp_clock.c` + man `clock_gettime(2)`/`clock_adjtime(2)` 核完再动手）——linux-pstore 教训：假设被内核源码推翻，返工一整段。
2. **开工即拿真实样例验证**——本机无 PHC，用**合成样例**：结构体字节流（构造/解析）、`sys_offset` 交错数组、`extts_event` 数组。`SIOCSHWTSTAMP` 可以用真实 loopback socket 测错误路径（返回 EOPNOTSUPP）。
3. **在 `/tmp` 开发实现**——按 DESIGN.md 的模块划分，从 `src/ioctl.rs`（注入边界）+ 纯结构体编解码起步，再 clock/offset → extts/perout（RAII）→ pin/pps → socktstamp，每个模块边写边补单元测试；不在正式目录里磨。
4. **集成测试**——happy path + **每个错误变体** + V2→V1 降级 + 容错；RAII 句柄 drop 释放验证。
5. **doc test 全覆盖**——每个公开 API 有可运行 doctest（ioctl 注入/合成数据）；真实硬件用法用 `no_run` doctest + 可选 example。
6. **质量门全绿**——见第 5 节命令；特性矩阵（默认 + `--no-default-features`）都要绿。
7. **仓库卫生**——见第 4 节清单与"当前仓库待修项"。
8. **一次干净提交**——/tmp 开发全绿后，一次性写入正式目录并 commit；后续修改用 `--amend` 合并。
9. **对照 DESIGN.md 清单 A–E 最终审计**——逐项给出证据，不是"看起来没问题"。

## 3. 两个 crate 的复盘：没做好的事（别再犯）

### 3.1 事实核实与真实样例
- **内核事实核实太晚 / 假设被推翻**：linux-pstore 的文件名格式、压缩算法两项假设都被内核源码推翻，返工一整段。→ 本项目 F1/F3/F4 已核（头文件逐字），**F2（时间读写路径）是最大的待核实项**，开工第一天必须核完：`ptp_clock.h` 里没有 GETTIME/SETTIME ioctl，很容易想当然用错 API。
- **真实语法验证太晚**：apparmor-policy 拿真实 profile 才测出 3 个漏掉的语法。→ 本项目没有真实设备，**fixture 就是一切**：结构体字节、`sys_offset` 数组、`extts_event` 数组的合成样例开工第一天就建。

### 3.2 解析器的细节坑（apparmor-policy 反复踩，全是真实 bug）
- **位置/偏移错位**：`sys_offset` 交错数组（2n+1 项，偶数位系统、奇数位 PHC、末尾系统）最容易算错——每个索引都必须有单测。
- **结构体布局想当然**：`ptp_clock_time.sec` 是 `i64`（符号作用于整体）、`nsec` 是 `u32` 非负；`reserved` 字段必须清零再发。`repr(C)` + `size_of` 断言。
- **终止/边界处理**：`extts_event` 从 fd 读回来是数组，一次 read 可能 0..N 个、可能跨 read——按 32 字节对齐，`EAGAIN` 映射 `Ok(None)`。
- **补丁应用必须验证**：改代码后立即构建验证，别信"以为应用了"。
- **属性测试优先**：结构体往返（构造→字节→解析==原值）是最强不变量。

### 3.3 仓库卫生（用户逐条指出，一开始就要做）
- `Cargo.lock` 不提交（库 crate），`.gitignore` 要有（`/target`、`/Cargo.lock`、编辑器杂项）。
- `DESIGN.md` 是过程文件：README 不引用、Cargo.toml 的 `include` 不含它。
- `LICENSE` 版权行用 `Copyright (c) 2026 <crate> contributors`（不是个人名），文本与同系列一致。
- `CHANGELOG.md`（Keep a Changelog）从一开始就要有，别等用户提醒。
- `Cargo.toml` 字段补全：`documentation`、`readme`、`authors`、`include`（含 CHANGELOG 与 tests，**不含 DESIGN.md**）、`[lib]`。
- CI 四 job（test 特性矩阵 / fmt / clippy / doc），`RUSTFLAGS="-D warnings"`。
- README 开头：标题 + 四个徽章链接（crates.io / docs.rs / License / CI，每个徽章一行），正文只写总览 + 一个主示例；`Cargo.toml` description 与 README 副标题保持精简一致。
- `.md` 格式：无意义硬换行一律去掉（段落/列表项各一行）；代码块、表格、标题、徽章等结构性换行必须保留。

## 4. ptp-clock 本项目要点（实现前必读）

### 4.1 本项目的核心难点与应对
1. **V2 优先 / V1 降级 + 标志校验**：同一 ioctl 有 V1/V2 两套编号；V2 严格校验标志（`PTP_STRICT_FLAGS` 内核恒强制）。先发 V2，`ENOTTY` 降级 V1；构造请求时按目标校验合法标志（V1 不接受 STRICT/EXT_OFFSET）。这是最容易做错的地方。
2. **`sys_offset` 数组语义**：`2*n_samples+1` 项交错 [sys, phc, ...]，**最后一项是系统时间戳**；`n_samples` 上限 `PTP_MAX_SAMPLES=25`。`sys_offset_extended` 每样本 [pre, PHC, post]，`clockid` 字段 6.12+（旧内核置 0）。
3. **RAII 句柄释放语义**：`ExtTsHandle` drop 时写 `PTP_EXTTS_REQUEST`（enable=0）释放通道；`PeroutHandle` drop 时写 period=0 禁用；释放失败记日志不 panic；`disarm()` 显式放弃。**drop 里绝不 set_time 等破坏性操作**。
4. **ioctl 注入边界（不回避硬件）**：ioctl 分发抽成可注入边界（默认 `libc::ioctl`，测试注入假实现）。请求构造、响应解析、数组解释、事件解析全部纯函数——**成功路径与错误路径都在本机测**，只留最终 syscall 命中给真机。真实用法给 `no_run` doctest + example。
5. **结构体布局**：`repr(C)`；`sec`=i64、`nsec`=u32、`reserved` 清零；`size_of` 断言（见 DESIGN 正确性要点）。
6. **`SIOCSHWTSTAMP` 降级语义**：返回的 `rx_filter` 可能被驱动降级（更通用模式），必须回读透传；用 `ifreq`（`ifr_name` + `ifr_data`）。

### 4.2 无硬件验证受限 → 测试策略（本机实测）
- 本机无 `/dev/ptp*`、无 PHC、无 root、无 sudo。**任何测试都不要依赖真实 PHC**。
- 全走可注入：ioctl 注入边界模拟成功/错误、结构体构造/解析纯测、`sys_offset`/`extts_event` 合成数据、`SIOCSHWTSTAMP` 用真实 loopback socket 测错误路径（`ENODEV`/`EOPNOTSUPP`——这是真实的、可测的错误路径）。
- 真实设备验证留给有硬件的用户：`no_run` doctest + 可选 `examples/`（列出能力、读一个 extts 事件）编译校验。
- 本机有 `/usr/include/linux/ptp_clock.h`、`net_tstamp.h`、`sockios.h`（内核 7.1.5）——ioctl 编号、结构体布局、标志位已逐字核实，实现时直接对照，不用联网。

### 4.3 止步点红线（本项目特有，写每个 API 前对照）
- **做**：能力门控、V2/V1 降级、ioctl 类型化、RAII 句柄、`sys_offset` 系列、引脚/PPS、`SIOCSHWTSTAMP` 配置、错误类型化、`AsFd` 逃生舱。
- **不做**：PTP 协议状态机、时钟伺服算法、NTP 互操作、PHC 自动发现/打分、`SO_TIMESTAMPING` 报文接收解析、线程/异步。
- **逃生舱**：`open_path`、`as_fd()`（事件队列直接 read）、原始 ioctl 透传入口。
- **别做过头**：不要在 `request_ext_ts` 里自动 set_pin_func（那是用户显式选择）；不要在 drop 里隐式清时间戳配置以外的任何东西；不要替用户决定读事件队列的时机。

### 4.4 当前仓库待修项（实现时必须一并修完）
- README.md 引用了 DESIGN.md 且没有四个徽章 → 重写为"总览 + 一个主示例 + 四个徽章"。
- Cargo.toml 的 `include` 含 `DESIGN.md` → 移除；补 `documentation` 字段。
- LICENSE 版权行是 `Copyright (c) 2026 lenitain` → 改为 `Copyright (c) 2026 ptp-clock contributors`。
- 缺 CHANGELOG.md、缺 `.github/workflows/ci.yml` → 新建。
- 当前 0 个 commit、全部文件 untracked → 实现完一次性提交。
- src/lib.rs 是占位骨架 → 按 DESIGN.md 模块划分重写。

## 5. 仓库卫生清单（提交前逐项勾）

- [ ] `cargo fmt --all -- --check` 绿
- [ ] `cargo clippy --all-targets -- -D warnings` 绿
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 绿（缺文档即红）
- [ ] `cargo test` 与 `cargo test --no-default-features` 都绿
- [ ] `#![warn(missing_docs)]` 常驻；每个公开 API 有可运行 doctest
- [ ] Cargo.toml：description/documentation/repository/readme/keywords/categories/authors/include/[lib]（include 含 CHANGELOG 与 tests，**不含 DESIGN.md**）
- [ ] CHANGELOG.md、LICENSE（contributors）、.gitignore（Cargo.lock 不提交）、CI 四 job
- [ ] README 开头有四个徽章链接（crates.io / docs.rs / License / CI，各一行）；正文只写总览 + 一个主示例；DESIGN.md 不进发布包、不被 README 引用
- [ ] .md 无意义硬换行已去除（段落/列表项各一行；代码块/表格/标题/徽章结构保留）
- [ ] 系统相关分支可测：ioctl 注入边界、结构体合成数据、loopback 错误路径（linux-pstore 的 `open_inner` 教训）
- [ ] 特性矩阵进 CI（默认 + 无默认）

## 6. 止步点自检（写每个 API 前问自己）

1. 这个 API 是不是在替上层决定"它怎么跑"（循环/线程/时机）？→ 是则不做。
2. 是否隐含了只适用部分场景的假设（"总是自动发现"、"总是缓存"）？→ 是则做成可选/可插拔。
3. 上层必须付出代价才能绕过我（改源码 / 开 unsafe / 重新解析我吞掉的信息）？→ 是则留逃生舱（`open_path` / `as_fd` / 原始 ioctl）。
4. 破坏性行为是否可能在用户不知情时发生？→ 是则改为显式（`set_time`/`adjust_freq`/`enable_pps`/`set_pin_func` 都只显式调用）。

正面参照（已完成 crate 的做法）：
- 数据永远给原始字节/原文；解析失败的内容降级保留，绝不丢数据。
- 清除/删除/修改绝不隐式发生，只有显式调用才执行。
- 系统相关探测抽成可注入函数，让系统分支也能单测（本项目扩展到 ioctl 注入边界）。

## 7. 红线

- 机制层做完整：正确性问题不能甩给用户。
- 政策层留白：不做偏好决定（事件循环、线程模型、缓存策略、资源发现策略、时钟选择策略、业务语义）。
- 数据不因"不识别"而丢失：能力/结果解析失败降级保留，绝不整体失败。
- 解析阶段不展开、不吞信息：结构体字段完整映射到类型化结果；`reserved` 不清零不发。
- 用往返不变量作为核心不变量：结构体构造→字节→解析==原值；`sys_offset` 数组布局大范围测试。
- 零依赖可用：`default-features = false` 必须能构建（核心只用 `libc`）。
- **本领域特有**：V2 优先、`ENOTTY` 降级 V1；`sys_offset` 交错数组 2n+1（末尾系统时间戳）；`SIOCSHWTSTAMP` 返回的 rx_filter 可能被降级必须透传；ioctl 错误（ENOTTY/EINVAL/EBUSY/EPERM）类型化；硬件路径不回避——ioctl 注入边界让成功/错误都能测。
