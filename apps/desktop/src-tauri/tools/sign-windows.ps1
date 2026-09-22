# Optional Authenticode hook for the Windows bundle.
#
# Tauri calls this once per bundled binary and once for the NSIS installer,
# through `bundle.windows.signCommand` in tauri.conf.json.
#
# It is inert by default. With no certificate configured it prints one line and
# exits 0, so an unsigned interim build succeeds unchanged. Signing happens only
# when the operator sets FETCHPATH_SIGN_THUMBPRINT to the SHA-1 thumbprint of a
# code-signing certificate already present in a Windows certificate store.
#
#   FETCHPATH_SIGN_THUMBPRINT    required to enable signing
#   FETCHPATH_SIGNTOOL           optional path to signtool.exe (default: signtool.exe on PATH)
#   FETCHPATH_SIGN_TIMESTAMP_URL optional RFC 3161 timestamp server
#   FETCHPATH_SIGN_STORE         optional certificate store name (default: the user store)
#
# This repository neither ships nor generates a certificate. See
# docs/development/WINDOWS-PACKAGING.md for the distribution decision.
#
# The command in tauri.conf.json is deliberately minimal, because Tauri writes
# it into the generated NSIS script for the uninstaller as
# `!uninstfinalize '<command>'`, a preprocessor directive that does not apply
# NSIS string escaping. Observed consequences, both reproduced during FP-017:
#
#   * an apostrophe in the command terminates the directive's argument and the
#     bundle fails with "!uninstfinalize expects 1-3 parameters";
#   * a `$` reaches the shell doubled, so `$hook` arrives as `$$hook` and the
#     command is a PowerShell parse error.
#
# So the command carries no quotes, no `$`, and no inline script: just
# `-File tools/sign-windows.ps1 %1`, relative to the `src-tauri` directory that
# the bundler runs from. The binary path therefore arrives unquoted, so a
# repository or output path containing spaces needs this reworked before signing
# is enabled.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $false, Position = 0)]
    [string] $BinaryPath
)

$ErrorActionPreference = 'Stop'

$thumbprint = $env:FETCHPATH_SIGN_THUMBPRINT
if ([string]::IsNullOrWhiteSpace($thumbprint)) {
    Write-Host 'fetchpath sign hook: FETCHPATH_SIGN_THUMBPRINT is not set; leaving this binary unsigned.'
    exit 0
}

if ([string]::IsNullOrWhiteSpace($BinaryPath) -or $BinaryPath -eq '%1') {
    Write-Error 'fetchpath sign hook: signing is enabled but no binary path was supplied.'
    exit 1
}
if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) {
    Write-Error "fetchpath sign hook: binary not found: $BinaryPath"
    exit 1
}

$signtool = $env:FETCHPATH_SIGNTOOL
if ([string]::IsNullOrWhiteSpace($signtool)) { $signtool = 'signtool.exe' }

$arguments = @('sign', '/sha1', $thumbprint, '/fd', 'sha256')
if (-not [string]::IsNullOrWhiteSpace($env:FETCHPATH_SIGN_STORE)) {
    $arguments += @('/s', $env:FETCHPATH_SIGN_STORE)
}
if (-not [string]::IsNullOrWhiteSpace($env:FETCHPATH_SIGN_TIMESTAMP_URL)) {
    $arguments += @('/tr', $env:FETCHPATH_SIGN_TIMESTAMP_URL, '/td', 'sha256')
}
$arguments += $BinaryPath

Write-Host "fetchpath sign hook: signing $BinaryPath"
& $signtool @arguments
if ($LASTEXITCODE -ne 0) {
    Write-Error "fetchpath sign hook: signtool failed with exit code $LASTEXITCODE."
    exit $LASTEXITCODE
}
exit 0
