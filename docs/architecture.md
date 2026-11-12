# 架构

WBuild 是一个用 Rust 编写的现代通用构建系统，核心目标是：**快速、简单、精准增量、本地缓存**。本页固化源码行为，供贡献者与深度用户参考。

## 模块划分

| 模块 | 职责 |
| --- | --- |
| `cli` | 命令行解析与子命令分派（clap 风格手写解析） |
| `config` | TOML 配置模型、默认值、profile 继承与校验 |
| `toolchain` | 编译器/链接器探测、版本识别、交叉编译、工具链指纹 |
| `plan` | 将配置目标转换为具体步骤与依赖 DAG |
| `graph` | DAG 校验与拓扑排序 |
| `exec` | 并行调度、指纹计算、缓存命中与 UpToDate 决策、依赖收集 |
| `state` | 增量状态持久化（state.json）与构建锁 |
| `cache` | 内容寻址缓存与依赖变体索引 |
| `hash` | BLAKE3 指纹、跨构建哈希索引与 stamp 备忘 |
| `depfile` | Make 风格 depfile 与 MSVC `/showIncludes` 解析 |
| `engine` | 构建流程编排、运行/测试、配置诊断、清理/列表/图/初始化 |
| `watch` | 源文件轮询（500ms）与自动重建 |
| `util` | 路径、原子写入、进程执行、终端颜色等工具 |

## 构建流水线

`wbuild build` 的完整执行顺序（`engine::build`）：

1. **Profile 解析** — 解析 `[build].profile` 及其 `extends` 继承链（最长 32 层，环形报错）。
2. **状态目录与锁** — 创建 `state_dir`（默认 `.wbuild/`），获取 `BuildLock`（同一项目同一时间仅允许一个构建进程）。
3. **读取 state.json** — 加载上次构建的步骤记录（fingerprint、deps、outputs、duration、updated）。
4. **工具链探测** — `Toolchain::detect_cached` 检测 cc/cxx/rustc/asm/ar/linker 及版本，结果进入指纹。
5. **Planner 生成 DAG** — `Planner::build` 将目标转换为步骤，建立依赖边，确定构建顺序。
6. **并发调度** — 并发数由 `-j` / `build.jobs` / 可用 CPU 决定；`exec::run` 并行执行步骤。
7. **缓存决策** — 对每个步骤按 Up-to-date → Cache hit → Built → Failed 决定（详见《增量与缓存》）。
8. **持久化** — 非 dry_run 时，持久化 hasher 备忘；将 Built/Cached 步骤合并进 state.json 并 prune 过期记录；state 与缓存均原子替换写入。
9. **时序报告** — 启用 `--timings` 或 `build.timings` 时，将 Top 10 步骤耗时写入 `state_dir/timings.json`（原子写入）。
10. **摘要输出** — 打印 `Finished X built, Y cached, Z up-to-date in <dur>` 及产物列表。

## 步骤类型

`plan::StepKind` 定义 9 种步骤，每种对应一个构建标签：

| 步骤类型 | 标签 | 说明 |
| --- | --- | --- |
| `CCompile` | CC | C 文件编译 |
| `CxxCompile` | CXX | C++ 文件编译 |
| `AsmCompile` | AS | 汇编文件编译 |
| `Rustc` | RUSTC | Rust crate 编译 |
| `Link` | LINK | 链接可执行/共享库 |
| `Archive` | AR | 静态库归档 |
| `Cargo` | CARGO | 通过 cargo 构建 |
| `Copy` | CP | 文件拷贝 |
| `Custom` | RUN | 自定义命令 |

每个步骤携带：目标名、种类、标签、程序、参数、环境变量、工作目录、输入、产物、额外产物、stdout 重定向、工具链指纹覆盖、depfile、依赖来源、是否可缓存、缓存扩展名、前置步骤 id 等。

## 设计约束

- **不使用 shell**：所有命令通过 `std::process::Command` 直接执行，参数不经 shell 字符串拼接，避免注入与跨平台差异。
- **指纹优先于 mtime**：增量判断基于 BLAKE3 内容指纹，不依赖文件修改时间。
- **内容寻址缓存**：产物按 fingerprint 存入 `objects/<2hex>/<hash>.<ext>`，配套 `.meta` 校验，缺失或不匹配条目自动丢弃。
- **原子写入**：state.json、timings.json、缓存条目均通过临时文件 + 重命名原子替换。
- **构建锁**：`BuildLock` 保证同一项目同一时间只有一个构建进程，避免状态并发损坏。
- **无 rustdoc**：源码中几乎不写 `///` 文档注释；行为规范固化在 `docs/` 下的 Markdown 文件中。

## 关键行为速查

| 行为 | 说明 |
| --- | --- |
| `clean` 默认仅删输出目录 | `--all` 才连同 `state_dir` 一起删除（含缓存） |
| `rebuild` = clean + build | 忽略现有缓存与状态 |
| `dry-run` 不写状态 | 不持久化 hasher，不写 state/timings |
| `quiet` 仅关闭 painter | 核心 `println` 输出仍保留 |
| `version` 可在项目外运行 | 只打印版本与宿主平台 |
| 缓存命中不执行命令 | 直接从 `objects/` 恢复产物，不产生新指纹 |
