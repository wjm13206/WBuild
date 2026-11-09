# WBuild benchmark harness.
#
# Generates an identical C project for each build system, then measures:
#   cold build, no-op rebuild, single-file incremental rebuild (median of -Runs).
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File bench/run-bench.ps1
#   powershell -ExecutionPolicy Bypass -File bench/run-bench.ps1 -Sources 200 -Runs 5 -Jobs 8
#
# Results are written to bench/RESULTS.md and bench/results.csv.
# Build context lives in bench/work (git-ignored).

param(
    [int]$Sources = 120,
    [int]$Jobs = 0,
    [int]$Runs = 3
)

$ErrorActionPreference = 'Stop'
$bench = $PSScriptRoot
$root = Split-Path -Parent $bench
$work = Join-Path $bench 'work'
$wbuild = Join-Path $root 'target/release/wbuild.exe'

if (-not (Test-Path $wbuild)) {
    throw "missing $wbuild - run: cargo build --release"
}

$cpu = (Get-CimInstance Win32_Processor).NumberOfLogicalProcessors
if ($Jobs -le 0) { $Jobs = $cpu }

$cc = (Get-Command gcc).Source
$ninja = (Get-Command ninja).Source
$cmake = (Get-Command cmake).Source
$make = (Get-Command mingw32-make -ErrorAction SilentlyContinue).Source

function Invoke-Silent {
    param([scriptblock]$Block, [string]$What)
    & $Block *> $null
    if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
}

function Measure-Build {
    param([scriptblock]$Block, [string]$What)
    $sw = [Diagnostics.Stopwatch]::StartNew()
    & $Block *> $null
    $sw.Stop()
    if ($LASTEXITCODE -ne 0) { throw "$What failed with exit code $LASTEXITCODE" }
    return [math]::Round($sw.Elapsed.TotalMilliseconds, 1)
}

function Get-Median {
    param([double[]]$Values)
    $sorted = $Values | Sort-Object
    $n = $sorted.Count
    if ($n % 2 -eq 1) { return $sorted[[int](($n - 1) / 2)] }
    return [math]::Round(($sorted[$n / 2 - 1] + $sorted[$n / 2]) / 2, 1)
}

# ---------------------------------------------------------------- generation

if (Test-Path $work) { Remove-Item $work -Recurse -Force }
$masterSrc = Join-Path $work 'src'
$masterInc = Join-Path $work 'include'
New-Item -ItemType Directory -Force -Path $masterSrc, $masterInc | Out-Null

@'
#ifndef SHARED_H
#define SHARED_H

static inline int scale(int x, int k) {
    return x * k + (k & 7);
}

#endif
'@ | Set-Content -Path (Join-Path $masterInc 'shared.h') -Encoding ascii

$mainLines = @('#include <stdio.h>', '#include "shared.h"')
for ($i = 0; $i -lt $Sources; $i++) {
    $name = 'gen_{0:D3}' -f $i
    $mainLines += "extern int $name(int);"
}
$mainLines += 'int main(void) {'
$mainLines += '    long sum = 0;'
for ($i = 0; $i -lt $Sources; $i++) {
    $name = 'gen_{0:D3}' -f $i
    $mainLines += "    sum += $name($i);"
}
$mainLines += '    printf("%ld\n", sum);'
$mainLines += '    return 0;'
$mainLines += '}'
$mainLines -join "`n" | Set-Content -Path (Join-Path $masterSrc 'main.c') -Encoding ascii

for ($i = 0; $i -lt $Sources; $i++) {
    $name = 'gen_{0:D3}' -f $i
    @"
#include "shared.h"

int $name(int x) {
    int y = scale(x, $($i + 1));
    return y + $i;
}
"@ | Set-Content -Path (Join-Path $masterSrc "$name.c") -Encoding ascii
}

function Initialize-Project {
    param([string]$Name)
    $dir = Join-Path $work $Name
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Copy-Item $masterSrc (Join-Path $dir 'src') -Recurse
    Copy-Item $masterInc (Join-Path $dir 'include') -Recurse
    return $dir
}

# ---------------------------------------------------------------- WBuild

$wdir = Initialize-Project 'wbuild'
@'
[project]
name = "bench"
out_dir = "build"

[target.app]
kind = "exe"
sources = ["src/*.c"]
include = ["include"]
'@ | Set-Content -Path (Join-Path $wdir 'wbuild.toml') -Encoding ascii

# ---------------------------------------------------------------- Make

$mdir = Initialize-Project 'make'
@'
CC := gcc
CFLAGS := -O0 -g -Iinclude -MMD -MP
SRCS := $(wildcard src/*.c)
OBJS := $(patsubst src/%.c,obj/%.o,$(SRCS))

app.exe: $(OBJS)
	$(CC) -o $@ $(OBJS)

obj/%.o: src/%.c
	$(CC) $(CFLAGS) -c $< -o $@

-include $(OBJS:.o=.d)

.PHONY: clean
clean:
	-del /q app.exe 2>nul
	-del /q obj\*.o 2>nul
	-del /q obj\*.d 2>nul
'@ | Set-Content -Path (Join-Path $mdir 'Makefile') -Encoding ascii
New-Item -ItemType Directory -Force -Path (Join-Path $mdir 'obj') | Out-Null

# ---------------------------------------------------------------- Ninja

$ndir = Initialize-Project 'ninja'
$ninjaLines = @(
    'rule cc',
    '  command = gcc -O0 -g -Iinclude -MMD -MF $out.d -c $in -o $out',
    '  depfile = $out.d',
    '  deps = gcc',
    '',
    'rule link',
    '  command = gcc -o $out $in',
    ''
)
$objNames = @()
for ($i = 0; $i -lt $Sources; $i++) {
    $name = 'gen_{0:D3}' -f $i
    $ninjaLines += "build obj/$name.o: cc src/$name.c"
    $objNames += "obj/$name.o"
}
$ninjaLines += 'build obj/main.o: cc src/main.c'
$objNames += 'obj/main.o'
$ninjaLines += ''
$ninjaLines += ('build app.exe: link ' + ($objNames -join ' '))
$ninjaLines += 'default app.exe'
$ninjaLines | Set-Content -Path (Join-Path $ndir 'build.ninja') -Encoding ascii
New-Item -ItemType Directory -Force -Path (Join-Path $ndir 'obj') | Out-Null

# ---------------------------------------------------------------- CMake + Ninja

$cdir = Initialize-Project 'cmake'
@'
cmake_minimum_required(VERSION 3.20)
project(bench C)
set(CMAKE_C_STANDARD 11)
file(GLOB SOURCES CONFIGURE_DEPENDS src/*.c)
add_executable(app ${SOURCES})
target_include_directories(app PRIVATE include)
target_compile_options(app PRIVATE -O0 -g)
'@ | Set-Content -Path (Join-Path $cdir 'CMakeLists.txt') -Encoding ascii

# ---------------------------------------------------------------- runners

$systems = [ordered]@{}

if ($make) {
    $systems['Make'] = @{
        Dir = $mdir
        Clean = { Invoke-Silent { & $make -C $mdir clean } 'make clean' }
        Build = { Measure-Build { & $make -C $mdir -j $Jobs } 'make' }
    }
}

$systems['Ninja'] = @{
    Dir = $ndir
    Clean = { Invoke-Silent { & $ninja -C $ndir -t clean } 'ninja clean' }
    Build = { Measure-Build { & $ninja -C $ndir -j $Jobs } 'ninja' }
}

$systems['CMake+Ninja'] = @{
    Dir = $cdir
    Clean = { Invoke-Silent { & $cmake --build (Join-Path $cdir 'build') --target clean } 'cmake clean' }
    Build = { Measure-Build { & $cmake --build (Join-Path $cdir 'build') --parallel $Jobs } 'cmake build' }
}

$systems['WBuild'] = @{
    Dir = $wdir
    Clean = {
        Invoke-Silent { & $wbuild -C $wdir clean --all } 'wbuild clean'
        Remove-Item (Join-Path $wdir 'build') -Recurse -Force -ErrorAction SilentlyContinue
    }
    Build = { Measure-Build { & $wbuild -C $wdir build -j $Jobs --no-cache } 'wbuild build' }
}

# ---------------------------------------------------------------- setup

Write-Host "WBuild benchmark: $Sources sources, jobs=$Jobs, runs=$Runs, cpu=$cpu"
$cmakeConfigure = $null
if ($cmake -and (Test-Path (Join-Path $cdir 'CMakeLists.txt'))) {
    $cmakeConfigure = Measure-Build {
        & $cmake -S $cdir -B (Join-Path $cdir 'build') -G Ninja -DCMAKE_BUILD_TYPE=Debug
    } 'cmake configure'
    Write-Host ("  cmake configure: {0} ms" -f $cmakeConfigure)
}

# Warm up the file system cache, then measure.
foreach ($name in $systems.Keys) {
    & $systems[$name].Clean
    & $systems[$name].Build | Out-Null
}

$results = [ordered]@{}
foreach ($name in $systems.Keys) {
    $results[$name] = [ordered]@{ cold = @(); noop = @(); incr = @() }
}

for ($run = 1; $run -le $Runs; $run++) {
    foreach ($name in $systems.Keys) {
        $sys = $systems[$name]
        & $sys.Clean
        $cold = & $sys.Build
        $noop = & $sys.Build

        # Mutate exactly one translation unit, then rebuild.
        $victim = Join-Path $sys.Dir 'src/gen_000.c'
        Add-Content -Path $victim -Value "/* incremental run $run */" -Encoding ascii
        $incr = & $sys.Build

        $results[$name].cold += $cold
        $results[$name].noop += $noop
        $results[$name].incr += $incr
        Write-Host ("  run {0}/{1} {2}: cold={3}ms noop={4}ms incr={5}ms" -f $run, $Runs, $name, $cold, $noop, $incr)
    }
}

# ---------------------------------------------------------------- WBuild cache reuse (extra)

# Populate the cache first, then measure restoring into a cleaned tree.
Invoke-Silent { & $wbuild -C $wdir clean --all } 'wbuild clean --all'
Invoke-Silent { & $wbuild -C $wdir build -j $Jobs } 'wbuild cache populate'

$cacheClean = @()
for ($run = 1; $run -le $Runs; $run++) {
    Invoke-Silent { & $wbuild -C $wdir clean } 'wbuild clean (cache kept)'
    $cacheClean += Measure-Build { & $wbuild -C $wdir build -j $Jobs } 'wbuild cache restore'
}
$cacheMedian = Get-Median $cacheClean

# ---------------------------------------------------------------- report

$gccVersion = (& gcc --version | Select-Object -First 1)
$ninjaVersion = (& ninja --version)
$cmakeVersion = ((& cmake --version | Select-Object -First 1) -replace '^cmake version ', '')
$wbuildVersion = (& $wbuild version | Select-Object -First 1)

$rows = @()
foreach ($name in $systems.Keys) {
    $rows += [pscustomobject]@{
        Tool = $name
        Cold = Get-Median ($results[$name].cold)
        NoOp = Get-Median ($results[$name].noop)
        Incr = Get-Median ($results[$name].incr)
    }
}

$rows | Format-Table -AutoSize | Out-String | Write-Host

$stamp = Get-Date -Format 'yyyy-MM-dd HH:mm:ss'
$md = @()
$md += '# WBuild Benchmark Results'
$md += ''
$md += "Generated: $stamp"
$md += ''
$md += '| Item | Value |'
$md += '| --- | --- |'
$md += "| CPU | $((Get-CimInstance Win32_Processor).Name) ($cpu logical cores) |"
$md += "| Jobs | $Jobs |"
$md += "| Sources | $Sources C files + main.c |"
$md += "| Runs | $Runs (median) |"
$md += "| Compiler | $gccVersion |"
$md += "| Ninja | $ninjaVersion |"
$md += "| CMake | $cmakeVersion |"
$md += "| WBuild | $wbuildVersion |"
$md += ''
$md += '| Tool | Cold build | No-op rebuild | 1-file incremental |'
$md += '| --- | ---: | ---: | ---: |'
foreach ($row in $rows) {
    $md += "| $($row.Tool) | $($row.Cold) ms | $($row.NoOp) ms | $($row.Incr) ms |"
}
$md += ''
$md += "WBuild `clean` (outputs removed, cache kept) then build: **$cacheMedian ms**."
$md += ''
$md += 'Notes:'
$md += ''
$md += '- All builds compile the same generated project with `gcc -O0 -g` and `-j ' + $Jobs + '`.'
$md += "- WBuild cold builds run with `--no-cache` for a fair comparison; the cache-reuse line measures the restore path."
if ($cmakeConfigure) {
    $md += "- CMake configure (`cmake -G Ninja`) took $cmakeConfigure ms once and is excluded from build timings."
}
$md += '- Timings include process startup. Only wall-clock build time is measured; no synthetic sleeps.'
$md | Set-Content -Path (Join-Path $bench 'RESULTS.md') -Encoding utf8

$csv = $rows | ForEach-Object {
    [pscustomobject]@{
        tool = $_.Tool; cold_ms = $_.Cold; noop_ms = $_.NoOp; incremental_ms = $_.Incr
    }
}
$csv | Export-Csv -Path (Join-Path $bench 'results.csv') -NoTypeInformation -Encoding utf8

Write-Host ''
Write-Host "wrote bench/RESULTS.md and bench/results.csv (cache-reuse: $cacheMedian ms)"