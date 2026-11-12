# 示例集

本页给出常见场景的 `wbuild.toml` 配置。所有路径均相对项目根目录。

## C 可执行程序

```toml
[project]
name = "hello"

[target.app]
kind = "exe"
sources = ["src/**/*.c"]
include = ["include"]
```

构建后产物位于 `build/debug/app.exe`（Windows）或 `build/debug/app`（类 Unix）。

## C++ 程序

```toml
[target.app]
kind = "exe"
sources = ["src/**/*.cpp"]
include = ["include"]
cxx_std = "17"
cxxflags = ["-Wall", "-Wextra", "-std=c++17"]
```

## 汇编（NASM）

```toml
[target.boot]
kind = "object"
sources = ["boot/start.nasm"]
asm_format = "bin"
output = "start.bin"

[target.app]
kind = "exe"
sources = ["src/main.c"]
links = ["boot"]
```

## 通过 rustc 构建 Rust crate

```toml
[target.app]
kind = "rust"
sources = ["src/main.rs"]
edition = "2021"
crate_type = "bin"
rustflags = ["-C", "link-arg=-s"]
```

`crate_type` 为空或 `"bin"` 时目标可被 `wbuild run` 直接执行。

## 通过 Cargo 构建

```toml
[target.app]
kind = "cargo"
manifest = "Cargo.toml"
features = ["default"]
cargo_args = ["--release"]
```

## 多目标与库依赖

```toml
[target.core]
kind = "staticlib"
sources = ["core/**/*.c"]
include = ["core/include"]

[target.app]
kind = "exe"
sources = ["src/**/*.c"]
include = ["include"]
links = ["core"]
```

`links` 声明的目标会先于当前目标构建，并参与链接。

## 自定义步骤（codegen 等）

```toml
[target.gen]
kind = "custom"
program = "tool"
args = ["--in", "a.txt", "--out", "gen/b.c"]
inputs = ["a.txt"]
outputs = ["gen/b.c", "gen/b.h"]
workdir = "sub"
stdout = "out/log.txt"
env = { FOO = "bar" }

[target.app]
kind = "exe"
sources = ["src/main.c", "gen/b.c"]
links = ["gen"]
```

- `program` 为必填；
- 至少需要一个 `outputs` 或 `stdout`；
- 首个 `outputs` 为主产物，其余作为额外产物；
- `links` 可让其它目标依赖此自定义步骤的产物。

## 多产物与交叉编译

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

## 测试目标

```toml
[target.core]
kind = "staticlib"
sources = ["core/*.c"]

[target.app_tests]
kind = "exe"
sources = ["tests/*.c"]
links = ["core"]
test = true
```

```console
$ wbuild test
test app_tests (build/debug/app_tests)
  ok
all 1 test target(s) passed
```

没有标记任何 `test = true` 时，`wbuild test` 会报错并退出码 `1`。

## 启用优化与 LTO（release profile）

```toml
[build]
profile = "release"

[profiles.release]
opt = 3
lto = true
debug = false
defines = ["NDEBUG"]
```

也可单次命令行覆盖：

```console
wbuild build -p release
```

## 完成示例（多语言混合项目）

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

[target.asm]
kind = "object"
sources = ["boot/start.nasm"]
asm_format = "elf64"
output = "start.o"

[target.app]
kind = "exe"
sources = ["src/**/*.c", "src/**/*.cpp"]
include = ["include"]
links = ["core", "asm"]
cflags = ["-Wall"]
cxxflags = ["-std=c++17"]

[target.gen]
kind = "custom"
program = "codegen"
args = ["--in", "spec.yaml", "--out", "gen/bindings.c"]
inputs = ["spec.yaml"]
outputs = ["gen/bindings.c"]

[target.app_tests]
kind = "exe"
sources = ["tests/*.c"]
links = ["core"]
test = true

[profiles.release]
opt = 3
lto = true
defines = ["NDEBUG"]

[toolchain.musl]
compiler = "musl-gcc"
target = "x86_64-linux-musl"
```
