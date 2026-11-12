# 开发

本页介绍如何在本地构建、测试 WBuild 本身，以及代码风格、CI 与发布约定。

## 环境要求

- Rust **1.89** 或更新版本
- Cargo 0.1.1（随 Rust 一起安装）

## 本地构建

```console
# 调试构建
cargo build

# 发布构建
cargo build --release

# 安装到本地
cargo install --path .
```

构建产物位于 `target/debug/wbuild` 或 `target/release/wbuild`。

## 测试

```console
# 运行全部测试
cargo test

# 只运行某个模块的测试
cargo test config
cargo test engine

# 文档示例（如有 rustdoc 测试）
cargo test --doc
```

WBuild 自身的测试位于 `tests/*.rs` 与各模块内的 `#[cfg(test)]` 模块（如 `config.rs` 中的配置解析、profile 继承、循环检测等测试）。

## 代码风格

- 采用 Rust 默认 `rustfmt` 与 `clippy` 风格，提交前请运行：

  ```console
  cargo fmt
  cargo clippy -- -D warnings
  ```

- 源码中**几乎不写 `///` rustdoc 注释**（设计约束之一）；行为规范固化在 `docs/` 下的 Markdown 文件中，与代码同步更新。
- 配置字段的默认值通过 `#[serde(default)]` 与 `Default` 实现，避免在解析逻辑中散布魔法值。
- 错误信息使用 `anyhow::bail!` 给出逐字可定位的消息（如 `target \`{name}\` cannot link to itself`），请保持此风格。

## 贡献流程

1. 从主分支创建特性分支。
2. 修改源码并补充对应的 `docs/` 页面。
3. 运行 `cargo test` 确保全部通过。
4. 提交 Pull Request，描述行为变更及对文档的影响。

## CI 与发布

- CI 在每次推送与 PR 时运行 `cargo test --all-targets` 与 `cargo clippy -- -D warnings`。
- 发布版本通过 `cargo publish` 发布到 crates.io；用户也可自行 `cargo install --path .` 安装。
- 当前版本为 `0.1.0`，版本号由 `Cargo.toml` 定义，`wbuild version` 打印时读取 `env!("CARGO_PKG_VERSION")`。
