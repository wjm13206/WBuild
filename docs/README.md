# WBuild 文档

> **Fast. Simple. Incremental.** — [返回项目首页](../README.md)

本目录是 WBuild 的完整文档。README 只保留概览与最短上手路径，细节都在这里。

## 目录

| 文档 | 内容 |
| --- | --- |
| [快速开始](getting-started.md) | 安装、第一个项目、目录结构、常用命令速览 |
| [配置参考](configuration.md) | `wbuild.toml` 全部字段：`[project]`、`[build]`、`[compiler]`、`[toolchain.*]`、`[profiles.*]`、`[target.*]` |
| [示例集](cookbook.md) | C / C++ / 汇编 / rustc / Cargo / 对象文件 / 自定义步骤 / 多产物 / 交叉编译 |
| [命令行参考](cli.md) | 每个子命令、构建选项、全局选项、输出格式与退出码 |
| [增量与缓存](incrementality.md) | 指纹构成、依赖追踪、内容寻址缓存、状态文件与构建锁 |
| [架构](architecture.md) | 模块划分、构建流水线、并发调度、设计约束 |
| [基准测试](benchmarks.md) | 测试方法、复现步骤与实测数据 |
| [开发](development.md) | 本地构建、测试、代码风格、CI 与发布 |

## 一分钟速览

```console
cargo install --path .        # 安装
wbuild init myapp             # 生成项目骨架
cd myapp
wbuild build                  # 构建
wbuild run -- --help          # 构建并运行
wbuild build -p release       # 优化构建
```

配置文件是项目根目录下的 `wbuild.toml`：

```toml
[project]
name = "hello"

[target.app]
kind = "exe"
sources = ["src/**/*.c"]
```
