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
    $Exe = if ($IsWindows -or $env:OS -eq 'Windows_NT') { 'dsh-tui.exe' } else { 'dsh-tui' }
    # Prefer a release build, fall back to the debug one. Requiring release meant a
    # first-time clone that had only run `cargo build` was told the binary did not exist.
    $Release = Join-Path (Join-Path 'target' 'release') $Exe
    if (Test-Path -PathType Leaf $Release) { $Release }
    else { Join-Path (Join-Path 'target' 'debug') $Exe }
}

if (-not (Test-Path -PathType Container $Harness)) {
    Write-Error "harness checkout not found: $Harness (set DSH_HARNESS)"
}
if (-not (Test-Path -PathType Leaf $Bin)) {
    Write-Error "dsh-tui binary not found: $Bin - run 'cargo build --release'"
}
if (-not (Test-Path -PathType Leaf 'bridge/lib/runner.js')) {
    Write-Error "bridge not built - run 'cd bridge; pnpm install --ignore-workspace; pnpm run build'"
}

# The next two are the checks this script used to lack, and both failed far downstream:
# without them the TUI came up and reported a bare disconnect, with the real cause only in
# the log. Name them here, where the fix is one command.
if (-not (Test-Path (Join-Path $Harness 'node_modules/.bin/tsx*'))) {
    Write-Error "harness dependencies not installed - run 'pnpm install' in $Harness"
}
if (-not (Test-Path -PathType Leaf (Join-Path $Harness 'packages/client/ui-user-questions/lib/client.js'))) {
    Write-Error "harness client libraries not built - run 'pnpm run build:lib' in $Harness ('dsh web' serves a client bundle; without it the host exits during startup.)"
}

$env:DSH_TUI_HOST_COMMAND = 'node'
$env:DSH_TUI_HOST_ARGS = '--import tsx/esm apps/cli/src/bin.ts web --no-open --port 0'
$env:DSH_TUI_HOST_CWD = $Harness

# `--runtime` consumes every remaining argument as the child command line, so the user's
# own flags must come before it.
& $Bin @args --runtime node bridge/lib/runner.js
exit $LASTEXITCODE
