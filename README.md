# WBuild

> **Fast. Simple. Incremental.**

WBuild 是一个用 Rust 编写的现代通用构建系统。它只读取一个 `wbuild.toml`，自动发现头文件依赖，用 BLAKE3 内容指纹做精准增量，并提供本地内容寻址缓存与并行 DAG 调度。所有工具都通过 `program + args` 直接执行，**不经过 shell**。

```toml
[project]
name = "hello"

[target.app]
kind = "exe"
sources = ["src/**/*.c"]
```

```console
$ wbuild build
WBuild 0.1.0  |  profile debug  | 5 steps | 12 jobs

[1/5] CC src/main.c                    187ms  built
...
Finished 5 built in 514ms
  -> app build/debug/app.exe
```

## 特性

- **快速**：Rust 实现，多线程并行执行，按 DAG 就绪顺序调度，并优先启动关键路径；失败后立即停止派发新任务。
- **简单**：全部配置都在 TOML 中，无需 CMake、Make 或脚本；常见项目只需几行。
- **精准增量**：BLAKE3 内容哈希 + 工具链身份 + 完整命令行 + 头文件依赖指纹，只重建真正受影响的步骤。
- **本地缓存**：内容寻址对象缓存，`wbuild clean` 后仍可复用；缓存键与依赖哈希分离，回退源码/头文件也能命中已有产物。
- **自动依赖追踪**：GCC/Clang 使用 `-MMD -MF -MP`，MSVC 使用 `/showIncludes`，rustc 使用 `--emit=dep-info`。
- **多目标**：`exe`、`staticlib`、`sharedlib`、`object`、`custom`、`rust`、`cargo`，目标之间用 `links` 表达依赖、链接顺序与头文件传递。
- **工具链管理**：自动探测 GCC、Clang、MSVC、rustc、cargo、ar、NASM，并支持显式路径和环境变量覆盖。
- **可诊断**：`--explain`、`--dry-run`、`--print-commands`、`--timings`、`wbuild doctor`、`wbuild graph`、`wbuild profile`、`wbuild cache`。
- **跨平台**：Windows / Linux / macOS；支持交叉编译目标三元组与 sysroot。
- **无 Shell**：命令始终以 `Command::new(program).args(args)` 执行，参数不会被二次解析，跨平台且安全。

## 安装

```console
cargo install --path .
# 或者
cargo build --release
```

需要 Rust 1.89 或更新版本（构建锁使用标准库的文件锁 API）。

编译产物为 `target/release/wbuild`（Windows 为 `wbuild.exe`）。

> Windows 提示：若只安装了 MSVC 工具链但缺少 `link.exe`，请改用 GNU 工具链：`rustup default stable-x86_64-pc-windows-gnu`，或在构建时设置 `RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-gnu`。

## 快速开始

```console
wbuild init myapp
cd myapp
wbuild build
./build/debug/app          # Windows: build\debug\app.exe
wbuild build -p release     # 优化构建
```

`wbuild init` 会生成一个可立即构建的 C 项目骨架（`wbuild.toml` + `src/main.c`）。

## 命令

| 命令 | 说明 |
| --- | --- |
| `wbuild build [TARGET...]` | 构建全部或指定目标 |
| `wbuild rebuild [TARGET...]` | 先清理再构建 |
| `wbuild run [TARGET] -- [ARGS...]` | 构建后运行可执行产物，`--` 之后的参数原样传入 |
| `wbuild test [TARGET...]` | 构建并运行所有 `test = true` 的目标 |
| `wbuild clean [--all]` | 删除输出目录；`--all` 同时删除状态与缓存 |
| `wbuild list` | 列出目标与依赖 |
| `wbuild configure` | 打印解析后的 profile、路径与目标，不执行构建 |
| `wbuild profile` | 读取 `.wbuild/timings.json`，列出最慢的步骤 |
| `wbuild cache [stats\|clean\|dir]` | 查看缓存统计、清空缓存或打印缓存目录 |
| `wbuild doctor` | 显示配置路径与检测到的工具链 |
| `wbuild graph [-o out.dot]` | 输出 Graphviz DOT 构建图 |
| `wbuild watch` | 轮询源文件并自动重建（500ms） |
| `wbuild version` | 打印版本与宿主平台 |
| `wbuild init [DIR] [--name N] [--force]` | 生成项目骨架 |

构建选项：`--force`、`--dry-run`、`--timings`、`--explain`、`--print-commands`、`--no-cache`。

全局选项：`-C/--directory DIR`、`-f/--file FILE`、`-j/--jobs N`、`-p/--profile NAME`、`-v/--verbose`、`-q/--quiet`、`--no-color`。

## 配置参考

### `[project]`

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `name` | 必填 | 项目名 |
| `version` | `"0.1.0"` | 版本号 |
| `root` | `"."` | 项目根目录（相对于配置文件） |
| `out_dir` | `"build"` | 构建产物目录 |
| `description` | 无 | 项目描述 |

### `[build]`

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `profile` | `"debug"` | 默认构建配置 |
| `jobs` | `0` | 并行任务数，`0` 表示自动检测 |
| `cache` | `true` | 启用本地对象缓存 |
| `state_dir` | `".wbuild"` | 状态、缓存与依赖文件目录 |
| `timings` | `false` | 总是记录性能报告 |

### `[compiler]`

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `cc` / `cxx` / `rustc` / `asm` / `ar` / `linker` | `"auto"` | 工具路径或名称；`auto` 自动探测 |
| `target` | 无 | 全局目标三元组，例如 `aarch64-linux-gnu` |
| `sysroot` | 无 | 交叉编译 sysroot |
| `extra_paths` | `[]` | 探测工具时优先搜索的目录 |
| `defines` | `[]` | 全局宏定义，如 `["FOO=1"]` |
| `include` | `[]` | 全局头文件目录 |
| `cflags` / `cxxflags` / `asmflags` / `rustflags` / `ldflags` | `[]` | 全局编译/链接参数 |

工具探测顺序：环境变量 `WBUILD_CC`、`WBUILD_CXX`、`WBUILD_RUSTC`、`WBUILD_ASM`、`WBUILD_AR`、`WBUILD_LINKER` → 配置中的显式路径 → `extra_paths` → `PATH`。交叉编译时会优先尝试带三元组前缀的工具（如 `aarch64-linux-gnu-gcc`）。

### `[profiles.<name>]`

内置 `debug`（`-O0 -g`）与 `release`（`-O2`、无调试信息、定义 `NDEBUG`）。自定义配置可通过 `extends` 继承。

| 字段 | 说明 |
| --- | --- |
| `extends` | 继承的配置名 |
| `opt` | 优化级别：`0`/`1`/`2`/`3`/`s`/`z` |
| `debug` | 是否生成调试信息 |
| `lto` | 是否启用 LTO（GCC/Clang `-flto`，rustc `-Clto=thin`） |
| `defines` / `cflags` / `cxxflags` / `asmflags` / `rustflags` / `ldflags` | 该配置附加参数 |

### `[target.<name>]`

| 字段 | 默认值 | 说明 |
| --- | --- | --- |
| `kind` | `"exe"` | `exe`、`staticlib`、`sharedlib`、`object`、`custom`、`rust`、`cargo` |
| `sources` | `[]` | 文件、目录（递归）或 glob；`object` 目标编译后不链接 |
| `exclude` | `[]` | 从源集合中排除的 glob/目录 |
| `include` | `[]` | 头文件目录；会沿 `links` 传递给消费者 |
| `defines` | `[]` | 目标级宏定义 |
| `cflags` / `cxxflags` / `asmflags` / `rustflags` / `ldflags` | `[]` | 目标级参数 |
| `links` | `[]` | 依赖的其他 WBuild 目标 |
| `libs` | `[]` | 系统库，如 `["m", "pthread"]` |
| `lib_dirs` | `[]` | 系统库搜索目录 |
| `cxx_std` | 无 | C++ 标准，如 `"17"`、`"20"` |
| `output` | 自动 | 显式产物路径（相对于 `out_dir`）；`object` 目标不支持 |
| `target` | 无 | 目标级交叉编译三元组 |
| `env` | `{}` | 执行该目标步骤时附加的环境变量 |
| `test` | `false` | 标记为测试可执行文件，供 `wbuild test` 运行 |

Rust 专用：`edition`（默认 `"2021"`）、`crate_type`（`bin`、`staticlib`、`cdylib`、`rlib`、`dylib`）。

Cargo 专用：`manifest`（必需，`Cargo.toml` 路径）、`features`、`cargo_args`。

Custom 专用：`program`（必需，名称或路径）、`args`、`inputs`（参与指纹的输入文件/glob）、`outputs`（必需，首个为主产物）。所有参数直接传给程序，不经过 shell。

## 示例

### C 可执行文件 + 静态库

```toml
[project]
name = "hello"
out_dir = "build"

[target.app]
kind = "exe"
sources = ["src/**/*.c"]
include = ["include"]
links = ["core"]

[target.core]
kind = "staticlib"
sources = ["core/*.c"]
include = ["core/include"]

[profiles.release]
opt = 2
debug = false
cflags = ["-Wall", "-Wextra"]
```

`app` 会自动获得 `core` 的 `include` 目录、链接 `libcore.a`，并保证构建顺序。动态库依赖还会自动加入运行期 rpath。

### C++ 共享库

```toml
[target.engine]
kind = "sharedlib"
sources = ["engine/**/*.cpp"]
include = ["engine/include"]
cxx_std = "20"
libs = ["m"]

[target.tool]
kind = "exe"
sources = ["tools/*.cpp"]
links = ["engine"]
```

### Assembly

```toml
[target.start]
kind = "staticlib"
sources = ["asm/*.asm"]
asmflags = ["-g"]
```

`asm` 工具自动使用 NASM 时，WBuild 会根据目标平台添加 `-f win64` / `elf64` / `macho64`。

### 直接使用 rustc

```toml
[target.app]
kind = "rust"
sources = ["src/main.rs"]
edition = "2021"
target = "x86_64-pc-windows-gnu"   # 需要对应的 Rust 标准库
```

### 包装 Cargo 项目

```toml
[target.app]
kind = "cargo"
manifest = "crate/Cargo.toml"
features = ["cli"]
```

WBuild 调用 `cargo build`，并把生成的可执行文件复制到 `build/<profile>/<name>[.exe]`。

### 对象文件目标

```toml
[target.piece]
kind = "object"
sources = ["src/piece.c"]

[target.app]
kind = "exe"
sources = ["src/main.c"]
links = ["piece"]
```

`object` 目标只编译不链接；对象文件写入 `build/<profile>/obj/<target>/`，并会被下游可执行文件或静态库自动加入链接输入。

### 自定义目标

```toml
[target.gen]
kind = "custom"
program = "protoc"
args = ["--cpp_out=gen", "proto/api.proto"]
inputs = ["proto/api.proto"]
outputs = ["gen/api.pb.cc", "gen/api.pb.h"]

[target.app]
sources = ["src/*.cc", "gen/api.pb.cc"]
include = ["gen"]
links = ["gen"]
```

`program` 可以是 `PATH` 中的名称，也可以是相对于项目根的路径。`inputs`（以及 `sources`）参与指纹，任一输入变化都会重新运行；`outputs` 的首项作为主产物，其余作为附加产物一起检查。其他目标通过 `links` 依赖 `gen` 后，可以直接把 `gen` 声明的输出写进自己的 `sources`/`include`——即使这些文件在首次规划时还不存在，WBuild 也会先完成生成步骤再编译消费者。

### 测试目标

```toml
[target.app_tests]
kind = "exe"
sources = ["tests/*.c"]
links = ["core"]
test = true
```

```console
$ wbuild test
...
test app_tests (build/debug/app_tests)
  ok
all 1 test target(s) passed
```

没有标记任何 `test = true` 时，`wbuild test` 会给出明确错误。

### 交叉编译

```toml
[compiler]
target = "aarch64-linux-gnu"
cc = "aarch64-linux-gnu-gcc"
cxx = "aarch64-linux-gnu-g++"
ar = "aarch64-linux-gnu-ar"
sysroot = "/opt/sysroot/aarch64"
```

也可以在单个目标上用 `target = "..."` 覆盖。

## 增量与缓存原理

每个步骤的指纹由以下内容组成：

1. 工具链身份（编译器路径、版本、目标三元组、sysroot）；
2. 步骤种类、程序路径、完整参数、环境变量与输出路径；
3. 所有静态输入文件的内容哈希（源文件等）；
4. 上一次构建发现的所有头文件/依赖文件路径及其**内容哈希**。

构建前用记录的依赖集合计算 `fp_pre`，与状态文件中保存的构建后指纹比较：完全一致且产物存在即 `up-to-date`，否则只重新执行该步骤。

缓存采用两级结构：

- **命令指纹**（不含依赖）索引多个构建变体；
- 每个变体记录依赖集合的哈希与产物指纹；
- 查找时先用当前依赖内容验证哈希，再原子地恢复产物。

因此：

- 修改头文件 → 只重建包含它的编译单元及下游链接步骤；
- 内容未变化的文件（例如只改注释）不会触发不必要的链接；
- 撤销一次修改 → 可直接命中此前缓存的产物；
- `wbuild clean`（保留 `.wbuild/cache`）后再次构建 → 仍能命中缓存。

状态与缓存写入均使用临时文件 + 原子替换；同一项目同时只允许一个构建进程（操作系统文件锁，进程退出后自动释放，崩溃或中断不会留下需要手工删除的死锁）。缺失的依赖文件会使指纹失配并触发重建，避免使用过期产物。

## 架构

| 模块 | 职责 |
| --- | --- |
| `cli` | 命令行解析与子命令分派 |
| `config` | TOML 配置模型、默认值、profile 继承与校验 |
| `toolchain` | 编译器/链接器探测、版本识别、交叉编译 |
| `plan` | 将目标转换为具体步骤与依赖 DAG |
| `graph` | DAG 校验与拓扑排序 |
| `exec` | 并行调度、指纹、缓存、依赖收集 |
| `state` | 增量状态持久化与构建锁 |
| `cache` | 内容寻址缓存与依赖变体索引 |
| `hash` | BLAKE3 指纹与文件哈希记忆化 |
| `depfile` | Make 风格 depfile 与 MSVC `/showIncludes` 解析 |
| `engine` | 构建流程编排、运行/测试、配置与缓存诊断、清理/列表/图/初始化 |
| `watch` | 源文件轮询与自动重建 |
| `util` | 路径、原子写入、进程执行、终端颜色等工具 |

## 开发与测试

```console
cargo test
cargo clippy --all-targets
```

测试包含单元测试与端到端 CLI 测试；端到端测试在没有 C 编译器时会自动跳过。

本仓库的示例：

- `examples/hello`：C 可执行文件 + 静态库，多目标、头文件传递、debug/release。
- `examples/rust-hello`：直接使用 `rustc` 构建。
- `examples/cargo-hello`：包装现有 Cargo 项目。

## 基准测试

`bench/run-bench.ps1` 生成同一个包含 120 个 C 翻译单元的项目，分别用 Make、Ninja、CMake+Ninja 与 WBuild 构建，取 3 次运行的中位数。完整数据与机器信息见 [`bench/RESULTS.md`](bench/RESULTS.md)。

实测环境：Intel Core 5 120U（12 逻辑核）、Windows、gcc 16.1.0（MinGW-W64 UCRT）、Ninja 1.13.2、CMake 4.4.1；统一使用 `-j 12` 与 `-O0 -g`。

| 工具 | 冷构建 | 无改动重建 | 单文件增量 |
| --- | ---: | ---: | ---: |
| Make | 3551 ms | 362 ms | 611 ms |
| Ninja | 3365 ms | 25 ms | 312 ms |
| CMake + Ninja | 3733 ms | 358 ms | 684 ms |
| **WBuild** | **3542 ms** | **73 ms** | **186 ms** |

冷构建与 Ninja 相差约 5%，差距主要来自 BLAKE3 内容指纹与依赖收集。无改动重建需要校验全部输入内容，因此慢于 Ninja（73 ms 对 25 ms），但仍远快于 Make 与 CMake+Ninja；单文件增量约为 Ninja 的 0.6 倍。`wbuild clean` 保留缓存后再构建耗时 168 ms，产物全部从本地缓存恢复。

复现：

```console
cargo build --release
powershell -ExecutionPolicy Bypass -File bench/run-bench.ps1 -Sources 120 -Runs 3
```

脚本只测量真实墙钟时间（包含进程启动），WBuild 冷构建使用 `--no-cache` 以保证对比公平；系统中没有可用 Make 时会自动跳过该行。

## 设计约束

- 配置只使用 TOML，不依赖 Make/CMake；`custom` 目标以 `program + args` 直接执行，不经过 shell。
- 不依赖 Make/CMake 作为构建引擎。
- 绝不拼接 shell 命令；每个参数单独传递。
- 增量正确性优先于缓存命中率：任何不确定因素都会导致重建。
- 依赖精简，核心只依赖 `anyhow`、`blake3`、`clap`、`crossbeam-channel`、`glob`、`serde`、`serde_json`、`toml`、`walkdir`。

---

**WBuild — Fast. Simple. Incremental.**