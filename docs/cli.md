# 命令行参考

WBuild 的命令行采用 `wbuild [全局选项] <子命令> [子命令选项] [参数]` 的形式。绝大多数子命令需要在包含 `wbuild.toml` 的项目目录（或其父目录）中运行；`version` 除外，可在项目外运行。

## 全局选项

| 选项 | 说明 |
| --- | --- |
| `-C` / `--directory DIR` | 指定项目目录（默认当前目录） |
| `-f` / `--file FILE` | 指定配置文件路径（默认向上查找 `wbuild.toml`） |
| `-j` / `--jobs N` | 并发步骤数；`0` 表示自动 |
| `-p` / `--profile NAME` | 使用的构建 profile |
| `-v` / `--verbose` | 打印每步命令与输出 |
| `-q` / `--quiet` | 关闭彩色进度行（核心 `println` 输出仍保留） |
| `--no-color` | 禁用 ANSI 颜色 |

## 子命令

| 子命令 | 说明 |
| --- | --- |
| `build [TARGET...]` | 构建全部或指定目标 |
| `rebuild [TARGET...]` | 先清理再构建 |
| `clean [--all]` | 删除输出目录；`--all` 同时删除状态、哈希索引与缓存 |
| `list` | 列出目标、种类、源码与链接 |
| `doctor` | 显示配置路径与检测到的工具链 |
| `graph [-o FILE]` | 输出 Graphviz DOT 构建图，`-o` 写入文件而非标准输出 |
| `watch` | 轮询源文件并自动重建（500ms 间隔） |
| `run [TARGET] -- [ARGS...]` | 构建后运行可执行产物；`--` 之后的参数原样传给子进程 |
| `test [TARGET...]` | 构建并运行所有 `test = true` 的目标 |
| `configure` | 打印解析后的配置，不执行构建 |
| `profile` | 读取 `.wbuild/timings.json`，列出最慢的步骤 |
| `cache [stats\|clean\|dir]` | 缓存统计 / 清空缓存 / 打印缓存目录 |
| `version` | 打印版本与宿主平台（可在项目外运行） |
| `init [DIR] [--name N] [--force]` | 生成项目骨架 |

### build

```
wbuild build [TARGET...] [--force] [--dry-run] [--timings] [--explain] [--print-commands] [--no-cache]
```

| 选项 | 说明 |
| --- | --- |
| `--force` | 强制重建所有步骤，忽略指纹与缓存 |
| `--dry-run` | 仅打印“将要构建”的步骤，不写入状态或 timings |
| `--timings` | 将每步耗时写入 `.wbuild/timings.json`（同时打印 Top 10） |
| `--explain` | 对每一步打印 `reason: <原因>` |
| `--print-commands` | 对每一步打印 `$ <command>` |
| `--no-cache` | 本次构建不读写缓存 |

输出示例：

```console
WBuild 0.1.0  |  profile debug  |  5 steps | 12 jobs

[1/5] CC src/main.c                    187ms  built
      $ cc -O0 -g -Isrc -c src/main.c -o build/debug/obj/main.o
[2/5] CC src/util.c                    42ms   built
[3/5] LINK app                         91ms   built
      -> app build/debug/app.exe

Finished 3 built, 2 up-to-date in 320ms
  -> app build/debug/app.exe
```

步骤状态：`built`（绿色，新构建）、`cached`（青色，命中缓存）、`up-to-date`（暗色，无需重建）、`would build`（青色，dry-run）、`FAILED`（红色，失败）。

`--explain` 会额外打印每一步重建的原因，例如输入文件变更、工具链版本变更、指令集或标志变更等。

退出码：构建成功 `0`，任一步骤失败 `1`。失败时打印 `error: N step(s) failed[, M not run]`。

### rebuild

等价于先 `wbuild clean` 后 `wbuild build`，忽略现有缓存与状态。

### clean

| 行为 | 说明 |
| --- | --- |
| 默认 | 删除 `out_dir`（默认 `build/`），保留缓存对象 |
| `--all` | 删除整个 `state_dir`（默认 `.wbuild/`），包括状态、哈希索引与缓存 |

输出 `removed <path>`；若无内容可清理则打印 `nothing to clean`。

> 注意：默认 `clean` 只删除输出目录，**保留**内容寻址缓存；若想彻底清除缓存，请使用 `--all` 或 `wbuild cache clean`。

### list

```console
demo v0.1.0 (C:/code/demo/wbuild.toml)

  app                 exe       src/main.c -> core
  core                staticlib core/*.c
```

每行格式：`<名称>    <种类>    <sources...> -> <links>`。

### doctor

```console
WBuild 0.1.0
  config    C:/code/demo/wbuild.toml
  root      C:/code/demo
  out_dir   C:/code/demo/build
  host      windows (x86_64)
  profile   debug

  cc        gcc (x86_64-w64-mingw32) 13.2.0
  cxx       g++ 13.2.0
  ar        llvm-ar 17.0.0
  linker    ld.lld 17.0.0
```

显示配置路径、根目录、输出目录、宿主平台、当前 profile 以及每项检测到的工具链标签与版本。

### graph

输出构建步骤的 Graphviz DOT 图（`rankdir=LR`，节点为步骤，边为依赖）。`-o FILE` 将图写入文件，否则打印到标准输出。

### run

```
wbuild run [TARGET] -- [ARGS...]
```

- 最多指定 **1 个** 目标；未指定时自动选择第一个可产生 artifact 的目标。
- `custom` 目标：先构建其 `links` 依赖，再运行 `program` 与配置的 `args`（不再重复执行构建步骤本身）；`--` 之后的参数追加到 `args` 后。
- 其它可运行目标：构建后运行其产物，工作目录为项目根。
- 输出 `running <name> <program|path>`。
- 退出码为子进程的退出码；构建失败时返回 `1`。

不可运行的 kind（`staticlib`、`sharedlib`、`object`、`cargo` 等非 artifact/非可执行）会报错：

> `target \`{name}\` is of kind \`{kind}\`, which cannot be executed`

### test

构建后运行所有标记 `test = true` 且可产生 artifact 的目标。每个目标输出：

```console
test app_tests (build/debug/app_tests)
  ok
```

失败示例：

```console
test app_tests (build/debug/app_tests)
  FAILED (exit code 1)
```

汇总：全部通过打印 `all N test target(s) passed`，否则 `N of M test target(s) failed`，退出码 `1`。若没有 `test = true` 的目标，报错：

> `no test targets configured; mark at least one executable target with \`test = true\``

### configure

不构建，打印已解析配置：

```console
demo v0.1.0
  config    C:/code/demo/wbuild.toml
  root      C:/code/demo
  out_dir   C:/code/demo/build
  profile   debug (opt=0, debug=true, lto=false)
  cache     enabled
  defines   NDEBUG

targets:
  app             exe,test    3 source(s)
  core            staticlib   2 source(s)
```

### profile

读取 `.wbuild/timings.json`（需先以 `--timings` 构建过），打印 profile 名、总耗时与最慢前 20 步。缺失时报：

> `no timing report at \`...\`; run \`wbuild build --timings\` first`

### cache

| 子操作 | 说明 |
| --- | --- |
| `stats` | 打印缓存根目录、对象数量与总字节、索引命令数 |
| `clean` | 清空缓存，输出 `cleared <root>` |
| `dir` | 打印缓存根目录路径 |

### version

```console
WBuild 0.1.0
  host      windows (x86_64)
  cache     content-addressed local cache
```

### init

```
wbuild init [DIR] [--name N] [--force]
```

生成项目骨架：在 `DIR`（默认当前目录）下写入 `wbuild.toml`、`src/` 与 `src/main.c`（C  Hello World），输出 `created <path>`，并提示运行 `wbuild build`。`--name` 指定项目名，`--force` 允许覆盖已存在的 `wbuild.toml`。

### 退出码

| 码 | 含义 |
| --- | --- |
| `0` | 成功 |
| `1` | 构建/运行/测试失败（`run` 返回子进程自身退出码） |
| 其他 | 配置错误、未找到 `wbuild.toml` 等 |
