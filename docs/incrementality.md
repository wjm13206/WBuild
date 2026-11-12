# 增量与缓存

WBuild 的增量构建基于 **BLAKE3 内容指纹**，不依赖文件修改时间（mtime），因此对触摸文件、切换分支、恢复旧版本等场景都能给出正确的增量判断。

## 构建流水线（简化）

```
profile 解析
    ↓
state_dir 创建 + BuildLock（同一项目同一时间只允许一个构建进程）
    ↓
读取 state.json（上次构建的步骤记录）
    ↓
Toolchain 探测（带缓存，工具链身份进入指纹）
    ↓
Planner 生成步骤 DAG
    ↓
按可用并发数并行执行步骤
    ↓
持久化 hasher 备忘；将 Built/Cached 步骤合并进 state.json 并 prune 过期记录
    ↓
（可选）写入 timings.json（Top 10，原子写入）
    ↓
打印摘要：Finished X built, Y cached, Z up-to-date in <dur>
```

state 与缓存均采用**原子替换**写入，避免崩溃时留下损坏文件。

## 指纹构成

每个步骤的指纹由以下域共同决定，任一域变化都会导致步骤重建：

| 指纹域 | 说明 |
| --- | --- |
| 命令行 | 步骤执行的完整命令（程序 + 参数 + 环境变量） |
| 工具链身份 | 编译器/链接器路径与版本（通过 `Toolchain::detect_cached` 获取） |
| 输入内容 | 步骤显式声明的 `inputs` 文件内容 |
| 依赖内容 | 自动发现的头文件等依赖内容（Makefile depfile / MSVC `/showIncludes`） |
| stdout 重定向 | 若 `stdout` 被重定向到文件，该文件路径也进入指纹 |

指纹计算使用 **BLAKE3**，并在 `compute_fingerprint` 中对依赖指纹做合并。命令指纹**刻意不包含**已发现的依赖集，以便依赖变更时能通过单独的依赖指纹校验正确失效。

## 增量决策表

每个步骤每次构建时按以下顺序判定：

1. **Up-to-date** — 状态文件中存在同 fingerprint 的记录，且所有输出文件仍存在 → 跳过，不产生新指纹。
2. **Cache hit** — 命中内容寻址缓存（`objects/<2hex>/<hash>.<ext>`），且依赖指纹与缓存条目一致 → 从缓存恢复产物，不再执行命令。
3. **Built** — 执行命令，将产物存入缓存，更新 state.json 记录。
4. **Failed** — 命令退出非零，保留 `fp_pre` 指纹，输出 exit code 与子进程输出。
5. **Dry-run / would-build** — `--dry-run` 时仅打印将要执行的步骤，不写入任何状态。

状态文件 `state.json` 记录每个步骤产物的 fingerprint、依赖列表、输出路径、耗时与更新时间；无实际变更的构建会保持 state 记录不变，避免无谓的磁盘写入。

## 缓存布局

缓存根目录为 `<state_dir>/cache`（默认 `.wbuild/cache`），采用内容寻址布局：

```
<state_dir>/
  cache/
    objects/
      <2-hex-prefix>/
        <fingerprint>.<ext>        # 产物副本
        <fingerprint>.<ext>.meta   # 元文件（原始路径、校验和）
    index/
      <fingerprint>.json           # 命令指纹 → 产物变体索引
```

- `objects/<2hex>/<hash>.<ext>`：以产物 fingerprint 的前两个字符做分片（shard），避免单目录文件过多。
- 每个缓存条目配套 `.meta` 文件，记录原始产物路径与内容校验和；读取时会校验，缺失或不匹配的条目会被丢弃（`discard`）。
- 缓存条目以**命令 fingerprint** 为键，一个命令可对应多个产物变体（通过 `index` 索引），变体间仅在依赖指纹不同时才会被视为不同。

## 状态文件

- 路径：`<state_dir>/state.json`
- 每条记录：`fingerprint`、`deps`、`outputs`、`duration_ms`、`updated`
- 构建结束时，只保留本次计划中仍存在的产物记录（`prune`），其余被移除；无实际变更时不重写文件。
- 构建锁 `BuildLock` 确保同一项目同一时间只有一个构建进程，避免状态文件并发损坏。

## 与清理的关系

| 操作 | 删除的内容 | 保留的内容 |
| --- | --- | --- |
| `wbuild clean` | 输出目录（默认 `build/`） | 缓存对象、状态文件 |
| `wbuild clean --all` | 整个 `state_dir`（含 `state.json`、缓存） | — |
| `wbuild cache clean` | 缓存根目录全部对象与索引 | 状态文件 |

> 因此，默认 `clean` 之后的下一次构建仍能命中之前的缓存；只有 `--all` 或 `cache clean` 会真正清空内容寻址缓存。

## 注意事项

- 指纹基于**内容**而非 mtime，所以 `touch` 源文件不会触发无谓重建；但文件内容实际变更（即使 mtime 回退）一定会被检测到。
- 工具链版本变更会导致所有相关步骤的指纹变更，从而触发全量重建，避免 stale 产物。
- `custom` 目标的 `program` 变更、参数变更、环境变量变更均会进入指纹，自动失效旧缓存。
- `--no-cache` 可单次禁用缓存读写，适合在干净环境下验证增量行为。
