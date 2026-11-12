# WBuild Benchmark Results

Generated: 2026-11-12 14:44:47

| Item | Value |
| --- | --- |
| CPU | Intel(R) Core(TM) 5 120U (12 logical cores) |
| Jobs | 12 |
| Sources | 120 C files + main.c |
| Runs | 3 (median) |
| Compiler | gcc.exe (MinGW-W64 x86_64-ucrt-posix-seh, built by Brecht Sanders, r4) 16.1.0 |
| Ninja | 1.13.2 |
| CMake | 4.4.1 |
| WBuild | WBuild 0.1.0 |

| Tool | Cold build | No-op rebuild | 1-file incremental |
| --- | ---: | ---: | ---: |
| Make | 4008.1 ms | 205.2 ms | 456.1 ms |
| Ninja | 4092.1 ms | 30.6 ms | 321.4 ms |
| CMake+Ninja | 4317.5 ms | 385.8 ms | 714.2 ms |
| WBuild | 4013.2 ms | 77.2 ms | 186.5 ms |

WBuild clean (outputs removed, cache kept) then build: **199.9 ms**.

Notes:

- All builds compile the same generated project with `gcc -O0 -g` and `-j 12`.
- WBuild cold builds run with --no-cache for a fair comparison; the cache-reuse line measures the restore path.
- CMake configure (cmake -G Ninja) took 1089.9 ms once and is excluded from build timings.
- Timings include process startup. Only wall-clock build time is measured; no synthetic sleeps.
