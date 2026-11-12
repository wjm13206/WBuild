# 配置参考

`wbuild.toml` 是项目的唯一配置文件。WBuild 会从当前目录**向上递归查找** `wbuild.toml`，找不到时报错：

> `could not find \`wbuild.toml\` in the current directory or any parent; run \`wbuild init\` to create one`

## 文件结构概览

```toml
[project]
name = "hello"
version = "0.1.0"
root = "."
out_dir = "build"
description = ""

[build]
profile = "debug"
jobs = 0          # 0 = 自动（可用 CPU 数）
cache = true
state_dir = ".wbuild"
timings = false

[compiler]
cc = "auto"
cxx = "auto"
rustc = "auto"
asm = "auto"
ar = "auto"
linker = "auto"
target = ""
sysroot = ""
extra_paths = []
defines = []
include = []
cflags = []
cxxflags = []
asmflags = []
rustflags = []
ldflags = []

[toolchain.<name>]
compiler = "..."
cc = "..."
cxx = "..."
rustc = "..."
asm = "..."
ar = "..."
linker = "..."
target = ""
sysroot = ""
extra_paths = []

[profiles.<name>]
extends = "debug"
opt = 0
debug = true
lto = false
defines = []
cflags = []
cxxflags = []
asmflags = []
rustflags = []
ldflags = []

[target.<name>]
kind = "exe"
sources = []
exclude = []
include = []
defines = []
cflags = []
cxxflags = []
asmflags = []
rustflags = []
ldflags = []
links = []
libs = []
lib_dirs = []
cxx_std = null
output = null
target = null
toolchain = null
compiler = null
cc = null
cxx = null
asm = null
ar = null
linker = null
workdir = null
stdout = null
asm_format = null
env = {}
edition = null
crate_type = null
manifest = null
features = []
cargo_args = []
program = null
args = []
inputs = []
outputs = []
test = false
```

字段以 `?` 标注为**可选**，其余视语义拥有默认值。数组字段缺省为空；布尔字段缺省见各节。

---

## [project]

| 字段 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `name` | `string` | (必填) | 项目名称，也用于 banner 与产物命名 |
| `version` | `string` | `"0.1.0"` | 项目版本 |
| `root` | `string` | `"."` | 项目根目录；源码路径相对此目录解析 |
| `out_dir` | `string` | `"build"` | 构造物输出目录（相对根目录） |
| `description` | `string` | `""` | 项目描述，`wbuild list`/`configure` 中显示 |

## [build]

| 字段 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `profile` | `string` | `"debug"` | 使用的 profile 名称 |
| `jobs` | `integer` | `0` | 并发步骤数；`0` 表示按可用 CPU 自动选择 |
| `cache` | `bool` | `true` | 是否启用内容寻址缓存 |
| `state_dir` | `string` | `".wbuild"` | 状态文件与缓存根目录（相对根目录） |
| `timings` | `bool` | `false` | 每次构建是否写入 `.wbuild/timings.json` |

`jobs` 的最终生效顺序：CLI `-j/--jobs` > `wbuild.toml` 的 `build.jobs` > 可用 CPU 数。

## [compiler]

全局工具链，可被 `[toolchain.<name>]` 预设与目标级 `compiler`/`cc`/`cxx` 等字段覆盖。

| 字段 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `cc` / `cxx` / `rustc` / `asm` / `ar` / `linker` | `string` | `"auto"` | 可执行文件名或路径；`"auto"` 表示自动探测 |
| `target` | `string` | `""` | 目标三元组（交叉编译） |
| `sysroot` | `string` | `""` | 系统根目录 |
| `extra_paths` | `array[string]` | `[]` | 额外查找工具链的目录 |
| `defines` | `array[string]` | `[]` | 全局预处理宏定义 |
| `include` | `array[string]` | `[]` | 全局头文件搜索路径 |
| `cflags` / `cxxflags` / `asmflags` / `rustflags` / `ldflags` | `array[string]` | `[]` | 各语言/链接阶段的额外标志 |

## [toolchain.<name>]

命名工具链预设，可被 `[target.<name>] toolchain = "..."` 引用。字段与 `[compiler]` 相同，且均可选。

示例（musl 交叉编译）：

```toml
[toolchain.musl]
compiler = "musl-gcc"
target = "x86_64-linux-musl"

[target.app]
sources = ["src/main.c"]
toolchain = "musl"
workdir = "sub"
stdout = "out/log.txt"
asm_format = "bin"
```

## [profiles.<name>]

Profile 支持**单向继承**：`extends` 引用另一个 profile，子 profile 的字段会覆盖父 profile。

- 继承链最长 **32 层**，超出时报 `profile inheritance is too deep at \`{name}\``；
- 出现环形继承时报 `profile inheritance cycle: ...`。

内置 profile（不可删除）：

| 名称 | opt | debug | lto | defines |
| --- | --- | --- | --- | --- |
| `debug` | `O0` | `true` | `false` | — |
| `release` | `O2` | `false` | `false` | `["NDEBUG"]` |

| 字段 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `extends` | `string` | — | 继承的父 profile 名 |
| `opt` | `string` | `0`（debug）/ `2`（release） | 优化等级：`0`, `1`, `2`, `3`, `s`, `z` |
| `debug` | `bool` | `true`（debug）/ `false`（release） | 是否生成调试信息 |
| `lto` | `bool` | `false` | 是否启用链接时优化 |
| `defines` | `array[string]` | `[]` | 该 profile 下追加的预处理宏 |
| `cflags` / `cxxflags` / `asmflags` / `rustflags` / `ldflags` | `array[string]` | `[]` | 该 profile 下追加的标志 |

继承示例：

```toml
[profiles.sanitize]
extends = "debug"
cflags = ["-fsanitize=address"]
```

## [target.<name>]

每个 `[target.<name>]` 表定义一个构建目标。`kind` 决定目标类型，其余字段按类型生效。

### kind 枚举

| 值 | 说明 |
| --- | --- |
| `exe` | 可执行程序（默认） |
| `staticlib` | 静态库 |
| `sharedlib` | 共享库（DLL/.so） |
| `object` | 单一目标文件 |
| `rust` | 通过 `rustc` 构建的 Rust crate |
| `cargo` | 通过 `cargo` 构建 |
| `custom` | 运行自定义命令（需 `program` 及至少一个 `outputs` 或 `stdout`） |

### 通用字段

| 字段 | 类型 | 默认 | 说明 |
| --- | --- | --- | --- |
| `kind` | `string` | `"exe"` | 目标种类 |
| `sources` | `array[string]` | `[]` | 源码 glob 列表（相对根目录） |
| `exclude` | `array[string]` | `[]` | 从 `sources` 中排除的 glob |
| `include` | `array[string]` | `[]` | 头文件搜索路径（相对根目录） |
| `defines` | `array[string]` | `[]` | 预处理宏定义 |
| `cflags` / `cxxflags` / `asmflags` / `rustflags` / `ldflags` | `array[string]` | `[]` | 目标级标志 |
| `links` | `array[string]` | `[]` | 依赖的其他 WBuild 目标名称 |
| `libs` | `array[string]` | `[]` | 系统库，如 `["m", "pthread"]` |
| `lib_dirs` | `array[string]` | `[]` | 库搜索目录 |
| `cxx_std` | `string?` | `null` | C++ 标准，如 `"17"` |
| `output` | `string?` | `null` | 显式产物名称或路径 |
| `target` | `string?` | `null` | 每目标目标三元组覆盖 |
| `toolchain` | `string?` | `null` | 引用 `[toolchain.<name>]` |
| `compiler` / `cc` / `cxx` / `asm` / `ar` / `linker` | `string?` | `null` | 每目标工具覆盖 |
| `workdir` | `string?` | `null` | custom 步骤的工作目录，默认项目根 |
| `stdout` | `string?` | `null` | custom 步骤将标准输出重定向到该文件 |
| `asm_format` | `string?` | `null` | NASM 输出格式（`elf64`、`win64`、`macho64`、`bin` …） |
| `env` | `map[string,string]` | `{}` | 步骤执行时的环境变量 |
| `test` | `bool` | `false` | 标记为测试目标，`wbuild test` 会运行它 |

### 类型特有字段

| 字段 | 适用 kind | 说明 |
| --- | --- | --- |
| `edition` | `rust` | Rust edition |
| `crate_type` | `rust` | crate 类型，`None` 或 `"bin"` 可直接运行 |
| `manifest` | `cargo` | Cargo.toml 路径，`kind = "cargo"` **必填** |
| `features` | `cargo` | cargo 特性列表 |
| `cargo_args` | `cargo` | 传给 cargo 的额外参数 |
| `program` | `custom` | 要执行的程序，**必填** |
| `args` | `custom` | 传给 `program` 的参数 |
| `inputs` | `custom` | 步骤输入文件（用于指纹与增量判断） |
| `outputs` | `custom` | 步骤产物，**至少一个产物或 `stdout`**；首个产物为主产物 |

### 校验规则（逐字）

- `target \`{name}\` cannot link to itself` — 不能将自身加入 `links`。
- `target \`{name}\`: kind "custom" requires \`program\``
- `target \`{name}\`: kind "custom" requires at least one output or \`stdout\``
- `target \`{name}\`: \`sources\` must not be empty` — `object` 以及所有非 custom 目标都需要 `sources`。
- `target \`{name}\`: kind "cargo" requires \`manifest\``
- `target \`{name}\`: kind "rust" requires exactly one source file (the crate root)`

### 可运行目标

`wbuild run` / `wbuild test` 需要可运行目标。可运行的 kind：`exe`、`cargo`、`custom`，以及 `rust`（`crate_type` 为空或 `"bin"`）。若未指定目标名，WBuild 会选第一个可产生 artifact 的目标；否则报：

> `no executable targets configured; declare one with \`kind = \"exe\"\` or pass a target name`

### 完整示例

```toml
[project]
name = "demo"
version = "0.1.0"
out_dir = "build"

[build]
profile = "debug"
jobs = 0
cache = true

[target.core]
kind = "staticlib"
sources = ["core/**/*.c"]
include = ["core/include"]

[target.app]
kind = "exe"
sources = ["src/**/*.c"]
include = ["include"]
links = ["core"]
cflags = ["-Wall", "-Wextra"]

[target.gen]
kind = "custom"
program = "tool"
args = ["--in", "a.txt", "--out", "gen/b.c"]
inputs = ["a.txt"]
outputs = ["gen/b.c", "gen/b.h"]

[target.app_tests]
kind = "exe"
sources = ["tests/*.c"]
links = ["core"]
test = true

[profiles.release]
opt = 3
defines = ["FAST=1"]

[toolchain.musl]
compiler = "musl-gcc"
target = "x86_64-linux-musl"

[target.cross]
sources = ["src/main.c"]
toolchain = "musl"
```

---

> 提示：`wbuild configure` 可在不构建的前提下打印已解析的配置与目标摘要，便于检查 `wbuild.toml` 是否按预期生效。
