#!/usr/bin/env pwsh
# Launch dsh-tui against a DSH source checkout. The PowerShell twin of run.sh.
#
# The bridge boots `dsh web` itself on an ephemeral loopback port; nothing needs to be
# running beforehand and no port is published.
$ErrorActionPreference = 'Stop'

$Harness = if ($env:DSH_HARNESS) { $env:DSH_HARNESS } else { Join-Path $HOME 'deepseek-harness' }
$Bin = if ($env:DSH_TUI_BIN) { $env:DSH_TUI_BIN } else {
    # Windows needs the extension; the same script serves pwsh on macOS and Linux, which
    # do not have one. `$IsWindows` exists only in PowerShell 6+, so Windows PowerShell
    # 5.1 - which runs nowhere else - is identified by `$env:OS` instead.
    if ($IsWindows -or $env:OS -eq 'Windows_NT') { '.\target\release\dsh-tui.exe' }
    else { './target/release/dsh-tui' }
}

if (-not (Test-Path -PathType Container $Harness)) {
    Write-Error "harness checkout not found: $Harness (set DSH_HARNESS)"
}
if (-not (Test-Path -PathType Leaf $Bin)) {
    Write-Error "binary not found: $Bin - run 'cargo build --release'"
}
if (-not (Test-Path -PathType Leaf 'bridge/lib/runner.js')) {
    Write-Error "bridge not built - run 'cd bridge; pnpm install --ignore-workspace; pnpm run build'"
}

$env:DSH_TUI_HOST_COMMAND = 'node'
$env:DSH_TUI_HOST_ARGS = '--import tsx/esm apps/cli/src/bin.ts web --no-open --port 0'
$env:DSH_TUI_HOST_CWD = $Harness

# `--runtime` consumes every remaining argument as the child command line, so the user's
# own flags must come before it.
& $Bin @args --runtime node bridge/lib/runner.js
exit $LASTEXITCODE
