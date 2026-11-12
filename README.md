# WBuild

> **Fast. Simple. Incremental.**

WBuild 是一个用 Rust 编写的现代通用构建系统。

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

- **快速**
- **简单**
- **精准增量**
- **本地缓存**
- **自动依赖追踪**
- **多目标**
- **工具链管理**
- **灵活的自定义步骤**
- **跨平台**

## 安装

```console
cargo install --path .
# 或者
cargo build --release
```

需要 Rust 1.89 或更新版本

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
| `wbuild run [TARGET] -- [ARGS...]` | 构建后运行可执行产物；`custom` 目标则先构建其 `links`，再运行 `program + args`，`--` 之后的参数原样传入 |
| `wbuild test [TARGET...]` | 构建并运行所有 `test = true` 的目标 |
| `wbuild clean [--all]` | 删除输出目录；`--all` 同时删除状态、哈希索引与缓存 |
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

详细命令行说明请参阅：[命令行参考](docs/cli.md)。

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

## 架构

WBuild 由 `cli`、`config`、`toolchain`、`plan`、`graph`、`exec`、`state`、`cache`、`hash`、`depfile`、`engine`、`watch`、`util` 等模块组成，详见：[架构](docs/architecture.md)。

## 基准测试

| 工具 | 冷构建 | 无改动重建 | 单文件增量 |
| --- | ---: | ---: | ---: |
| Make | 4008 ms | 205 ms | 456 ms |
| Ninja | 4092 ms | 31 ms | 321 ms |
| CMake + Ninja | 4318 ms | 386 ms | 714 ms |
| **WBuild** | **4013 ms** | **77 ms** | **187 ms** |

详见：[基准测试](docs/benchmarks.md)。

## 完整文档

全部中文文档位于 `docs/` 目录：

- [快速开始](docs/getting-started.md)
- [配置参考](docs/configuration.md)
- [示例集](docs/cookbook.md)
- [命令行参考](docs/cli.md)
- [增量与缓存](docs/incrementality.md)
- [架构](docs/architecture.md)
- [基准测试](docs/benchmarks.md)
- [开发](docs/development.md)

索引页：[docs/README.md](docs/README.md)

**WBuild — Fast. Simple. Incremental.**
