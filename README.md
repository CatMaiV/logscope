# LogScope

本地、离线、只读的静态日志文件可视化分析器。面向 Java / Spring Boot 开发者：双击导入 Logback 日志，
在时间轴上框选异常高峰、过滤检索、查看完整堆栈与前后文。无后台服务、无数据库、无 JRE 依赖。

> 需求与架构见 [`doc/LogScope_开发计划_V1.0.md`](doc/LogScope_开发计划_V1.0.md)。

## 技术栈

- **Rust 核心**（`crates/logscope-core`）：扫描、解析、索引、检索；原文不整体载入内存，仅存字节偏移
- **Tauri 2 桌面壳**（`src-tauri`）：命令 + Channel 进度/取消
- **Vue 3 + TypeScript + Vite 前端**（`src/`）
- **CLI 验证工具**（`crates/logscope-cli`）：核心能力的命令行验收路径

## 开发环境

- Rust stable（MSVC toolchain）
- Node 22+
- Windows: VS Build Tools（C++ 工作负载）+ WebView2

## 快速开始

```bash
npm install                # 前端依赖
npm run tauri dev          # 启动桌面应用
```

CLI 验证核心能力（无需 UI）：

```bash
cargo run -p logscope-cli -- parse fixtures/basic_app.log
cargo run -p logscope-cli -- parse fixtures/multiline_stacktrace.log   # 多行堆栈 = 1 事件
cargo run -p logscope-cli -- show fixtures/multiline_stacktrace.log --id 1
cargo run -p logscope-cli -- context fixtures/basic_app.log --id 10 --before 3 --after 3
cargo run -p logscope-cli -- filter fixtures/basic_app.log --level error,warn --text 支付
cargo run -p logscope-cli -- timeline fixtures/basic_app.log
```

生成基准数据（输出到 `target/`，不入库）：

```bash
cargo run -p logscope-cli -- gen target/bench_100m.log --size 100MB --seed 42
cargo run -p logscope-cli -- parse target/bench_100m.log
```

夹具的特殊二进制形态（CRLF / BOM / 超长行 / 超大事件 / GBK）：

```bash
node scripts/prepare-fixtures.mjs
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/gen-gbk-fixture.ps1   # Windows
```

## 测试

```bash
cargo test -p logscope-core -p logscope-cli   # 核心单测（含夹具集成）
cargo clippy --workspace --all-targets -- -D warnings
npm run build                                 # 前端 typecheck + build
```

## 性能实测（M1，本机 NVMe，release 构建）

实测数据非承诺值，详见开发计划 §8 的目标口径。

| 场景 | 实测（2026-11-20，Rust 1.100，release -j 单线程扫描） |
| --- | --- |
| 100MB（83 万事件） | **0.78s**（目标：尽量 10s 内） |
| 1GB（828 万事件） | **12.2s**，峰值内存 473MB（目标：90s 内；内存按事件数≈40B/事件增长，不随文件字节线性） |

## 路线图

- [x] M0 工程基础：workspace、夹具、CI
- [x] M1 Rust 解析与索引：扫描 / Logback 解析 / 多行事件 / 内存索引 / 原文按需读取 / CLI 验收
- [ ] M2 检索与时间轴引擎（正则、取消、游标、基准）
- [ ] M3 可视化 MVP（Naive UI + ECharts 时间轴 + 虚拟滚动列表）
- [ ] M4 多文件稳定性与发布（自定义 Pattern、文件变化检测、安装包）

## 许可

MIT
