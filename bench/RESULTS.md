# WBuild Benchmark Results

Generated: 2026-11-08 21:25:14

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
| Make | 3551.3 ms | 361.9 ms | 610.5 ms |
| Ninja | 3365 ms | 25.2 ms | 312.4 ms |
| CMake+Ninja | 3732.5 ms | 357.7 ms | 684.1 ms |
| WBuild | 3541.5 ms | 73.1 ms | 185.7 ms |

WBuild clean (outputs removed, cache kept) then build: **167.6 ms**.

Notes:

- All builds compile the same generated project with `gcc -O0 -g` and `-j 12`.
- WBuild cold builds run with --no-cache for a fair comparison; the cache-reuse line measures the restore path.
- CMake configure (cmake -G Ninja) took 1091.9 ms once and is excluded from build timings.
- Timings include process startup. Only wall-clock build time is measured; no synthetic sleeps.
