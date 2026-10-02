# Fetchpath clean-machine lifecycle in Windows Sandbox (FP-043, A12).
#
# Starts a fresh, disposable Windows Sandbox, runs sandbox-inner.ps1 in it as
# the logon command, waits for its result, closes the Sandbox and records the
# result as evidence. Nothing is installed on the host.
#
# Requires the Windows Sandbox optional feature and its `wsb` command, and the
# release installer built by `corepack pnpm --dir apps/desktop release`.
#
# Emits JSON to -OutputPath and throws on any failure.

[CmdletBinding()]
param(
    [string] $InstallerPath,
    [string] $OutputPath,
    [int] $TimeoutMinutes = 20,
    # FP-100: the uninstall step passes /DELETEAPPDATA and checks the removal.
    [switch] $DeleteData
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
if (-not $InstallerPath) {
    $InstallerPath = Join-Path $repositoryRoot 'target\release\bundle\nsis\Fetchpath_0.1.0_x64-setup.exe'
}
if (-not $OutputPath) {
    $OutputPath = Join-Path $repositoryRoot 'docs\development\evidence\windows\sandbox-lifecycle.json'
}
if (-not (Test-Path -LiteralPath $InstallerPath -PathType Leaf)) { throw "Installer not found: $InstallerPath" }
if (-not (Get-Command wsb -ErrorAction SilentlyContinue)) { throw 'The Windows Sandbox command `wsb` is not available.' }
# `wsb list --raw` prints {"WindowsSandboxEnvironments": [...]}.
function Get-RunningSandboxIds {
    $listing = (& wsb list --raw | Out-String) | ConvertFrom-Json
    return , @($listing.WindowsSandboxEnvironments | ForEach-Object { if ($_ -is [string]) { $_ } else { $_.Id } } | Where-Object { $_ })
}
if ((Get-RunningSandboxIds).Count -gt 0) {
    throw 'A Windows Sandbox is already running; only one can run at a time. Close it first.'
}

# The staging folders live under work/, which Git ignores.
$stage = Join-Path $repositoryRoot 'work\fp043-sandbox'
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
$inputDirectory = Join-Path $stage 'in'
$resultDirectory = Join-Path $stage 'out'
[System.IO.Directory]::CreateDirectory($inputDirectory) | Out-Null
[System.IO.Directory]::CreateDirectory($resultDirectory) | Out-Null
$installerName = [System.IO.Path]::GetFileName($InstallerPath)
Copy-Item -LiteralPath $InstallerPath -Destination $inputDirectory
foreach ($script in @('sandbox-inner.ps1', 'uia-common.ps1')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $script) -Destination $inputDirectory
}

$sandboxIn = 'C:\fp\in'
$sandboxOut = 'C:\fp\out'
$logon = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File $sandboxIn\sandbox-inner.ps1 " +
    "-InstallerPath $sandboxIn\$installerName -OutputPath $sandboxOut\result.json" +
    $(if ($DeleteData) { ' -DeleteData' } else { '' })
$configuration = @"
<Configuration>
  <MappedFolders>
    <MappedFolder><HostFolder>$inputDirectory</HostFolder><SandboxFolder>$sandboxIn</SandboxFolder><ReadOnly>true</ReadOnly></MappedFolder>
    <MappedFolder><HostFolder>$resultDirectory</HostFolder><SandboxFolder>$sandboxOut</SandboxFolder><ReadOnly>false</ReadOnly></MappedFolder>
  </MappedFolders>
  <LogonCommand><Command>$logon</Command></LogonCommand>
  <ClipboardRedirection>Disable</ClipboardRedirection>
  <PrinterRedirection>Disable</PrinterRedirection>
</Configuration>
"@
$configurationPath = Join-Path $stage 'fetchpath.wsb'
[System.IO.File]::WriteAllText($configurationPath, $configuration, [System.Text.UTF8Encoding]::new($false))

# Opening the .wsb file, rather than `wsb start`, gives the Sandbox an
# interactive logon session: the logon command runs in it and the desktop app
# gets a real window for UI Automation to drive.
$started = [DateTime]::UtcNow
Start-Process -FilePath $configurationPath
$doneMarker = Join-Path $resultDirectory 'done'
$deadline = $started.AddMinutes($TimeoutMinutes)
while (-not (Test-Path -LiteralPath $doneMarker) -and [DateTime]::UtcNow -lt $deadline) {
    Start-Sleep -Seconds 5
}
$finished = Test-Path -LiteralPath $doneMarker

foreach ($id in Get-RunningSandboxIds) { & wsb stop --id $id | Out-Null }

if (-not $finished) { throw "The Sandbox run did not finish within $TimeoutMinutes minutes. Staging: $stage" }
$result = Get-Content -LiteralPath (Join-Path $resultDirectory 'result.json') -Raw | ConvertFrom-Json
$result | Add-Member -NotePropertyName hostOsBuild -NotePropertyValue ([string] [System.Environment]::OSVersion.Version)
$result | Add-Member -NotePropertyName recordedAt -NotePropertyValue ([DateTime]::UtcNow.ToString('o'))
$result | Add-Member -NotePropertyName wallClockSeconds -NotePropertyValue ([Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds))
$json = $result | ConvertTo-Json -Depth 8
[System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($OutputPath)) | Out-Null
[System.IO.File]::WriteAllText($OutputPath, $json, [System.Text.UTF8Encoding]::new($false))
$json

if (-not $result.passed) {
    throw "Sandbox lifecycle checks failed:`n - $(@($result.failures) -join "`n - ")"
}
