# 基准测试

本页记录 WBuild 的构建性能测试方法、复现步骤与实测数据。

## 测试方法

所有基准在同一机器上按以下场景测量：

1. **冷构建**：从无任何产物与缓存的状态下首次完整构建。
2. **无改动重建**：不修改任何源码，再次构建，预期全部命中缓存或 up-to-date。
3. **单文件增量**：修改单个源文件，仅重建受影响的步骤及其下游依赖。

使用 `wbuild build --timings` 捕获每步耗时，结果写入 `.wbuild/timings.json`，并可通过 `wbuild profile` 查看最慢步骤。

## 复现步骤

```console
# 1. 在项目根目录确认存在 wbuild.toml
wbuild init bench          # 或使用已有示例项目
cd bench

# 2. 冷构建（清除状态后）
wbuild clean --all
wbuild build --timings

# 3. 无改动重建
wbuild build --timings

# 4. 单文件增量
# 修改 src/main.c 中一行，再构建
wbuild build --timings

# 5. 查看最慢步骤
wbuild profile
```

## 实测数据

| 工具 | 冷构建 | 无改动重建 | 单文件增量 |
| --- | ---: | ---: | ---: |
| Make | 4008 ms | 205 ms | 456 ms |
| Ninja | 4092 ms | 31 ms | 321 ms |
| CMake + Ninja | 4318 ms | 386 ms | 714 ms |
| **WBuild** | **4013 ms** | **77 ms** | **187 ms** |

数据来源：`bench/RESULTS.md` 与 `bench/results.csv`（仓库 `bench/` 目录）。测试项目为多目标 C/C++ 混合项目，包含静态库依赖、汇编目标文件与自定义代码生成步骤。

## 结果解读

- **冷构建**：WBuild 与 Make、Ninja 同量级（约 4 s），说明并发调度与工具链探测开销可控。
- **无改动重建**：77 ms，远优于 Make（205 ms）与 CMake+Ninja（386 ms），略高于 Ninja（31 ms，Ninja 的无状态快速路径更轻）。
- **单文件增量**：187 ms，受益于 BLAKE3 内容指纹与自动依赖追踪，仅重建受影响步骤及其下游。

> 基准数据会随硬件、工具链版本与项目规模变化，仅供参考；建议在目标环境下用上述步骤自行复现。
