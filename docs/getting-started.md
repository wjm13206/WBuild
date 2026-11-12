# 快速开始

本页引导你在几分钟内安装 WBuild、创建第一个项目并完成一次增量构建。

## 安装

```console
cargo install --path .
# 或者先构建再安装
cargo build --release
cargo install --path .
```

需要 Rust 1.89 或更新版本。安装后，`wbuild` 命令即可在任意目录使用；`wbuild version` 可在项目外运行，用于确认版本：

```console
wbuild version
```

## 创建第一个项目

```console
wbuild init myapp
cd myapp
```

`wbuild init` 会生成以下骨架：

```
myapp/
├── wbuild.toml
└── src/
    └── main.c
```

`wbuild.toml` 内容示例：

```toml
[project]
name = "myapp"
version = "0.1.0"
out_dir = "build"

[target.app]
kind = "exe"
sources = ["src/**/*.c"]
include = ["include"]
```

`src/main.c` 是一个可立即编译的 C Hello World。

## 构建与运行

```console
wbuild build
```

预期输出：

```console
WBuild 0.1.0  |  profile debug  |  5 steps | 12 jobs

[1/5] CC src/main.c                    187ms  built
[2/5] LINK myapp                       42ms   built
      -> myapp build/debug/myapp.exe

Finished 2 built in 229ms
  -> myapp build/debug/myapp.exe
```

运行产物：

```console
# 类 Unix
./build/debug/myapp

# Windows
build\debug\myapp.exe
```

或者一步完成构建并运行：

```console
wbuild run
```

## 使用 release profile

```console
wbuild build -p release
```

等价于在 `wbuild.toml` 中将 `build.profile` 设为 `"release"`，会使用优化级 O2 并定义 `NDEBUG`。

## 常用命令速览

| 命令 | 作用 |
| --- | --- |
| `wbuild build` | 构建全部目标 |
| `wbuild build app` | 仅构建指定目标 |
| `wbuild rebuild` | 先清理输出目录，再构建 |
| `wbuild clean` | 删除输出目录（保留缓存） |
| `wbuild clean --all` | 删除整个状态目录（含缓存） |
| `wbuild run` | 构建后运行第一个可执行目标 |
| `wbuild run app -- --help` | 运行目标并透传参数 |
| `wbuild test` | 构建并运行所有 `test = true` 目标 |
| `wbuild list` | 列出目标、种类与源码 |
| `wbuild configure` | 打印已解析配置，不执行构建 |
| `wbuild doctor` | 显示配置路径与检测到的工具链 |
| `wbuild cache stats` | 查看缓存对象数与总字节 |
| `wbuild graph -o out.dot` | 输出 Graphviz 构建图 |
| `wbuild watch` | 源文件变化时自动重建（500ms 轮询） |
| `wbuild version` | 打印版本与宿主平台 |

更多子命令与选项请参阅：[命令行参考](cli.md)。

## 下一步

- 配置全部字段：[配置参考](configuration.md)
- 常见场景示例：[示例集](cookbook.md)
- 增量构建与缓存原理：[增量与缓存](incrementality.md)
- 模块划分与构建流水线：[架构](architecture.md)

## 目录结构示例

一个典型的 WBuild 项目：

```
myproject/
├── wbuild.toml          # 项目配置
├── src/                 # 源码
│   ├── main.c
│   └── util.c
├── include/             # 公开头文件
│   └── util.h
├── tests/               # 测试源码
│   └── app_tests.c
├── core/                # 静态库源码
│   └── core.c
└── build/               # 构造物输出目录（默认，可配置）
    └── debug/
        ├── myapp.exe
        └── ...
```

状态文件与缓存默认位于 `.wbuild/`（可通过 `build.state_dir` 配置），不应提交到版本控制。
