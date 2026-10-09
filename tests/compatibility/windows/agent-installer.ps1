# FP-102: disposable installer lifecycle; never installs on the owner's host.
[CmdletBinding()]
param([string]$InstallerPath, [string]$OutputPath, [int]$TimeoutMinutes = 15)
$ErrorActionPreference = 'Stop'
$repo = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
if (-not $InstallerPath) { $InstallerPath = Join-Path $repo 'target/release/bundle/nsis/Fetchpath_0.3.0_x64-setup.exe' }
if (-not $OutputPath) { $OutputPath = Join-Path $repo 'docs/development/evidence/windows/agent-installer.json' }
if (-not (Test-Path -LiteralPath $InstallerPath)) { throw 'Build the current release installer first' }
function Sandbox-Ids {
    $listing = (& wsb list --raw | Out-String) | ConvertFrom-Json
    return ,@($listing.WindowsSandboxEnvironments | ForEach-Object { if ($_ -is [string]) { $_ } else { $_.Id } })
}
if ((Sandbox-Ids).Count -ne 0) { throw 'Close the existing Windows Sandbox first' }
$stage = Join-Path $repo ('work/fp102-sandbox-' + [Guid]::NewGuid().ToString('N'))
$inputDir = Join-Path $stage 'in'
$outputDir = Join-Path $stage 'out'
New-Item -ItemType Directory -Path $inputDir,$outputDir -Force | Out-Null
Copy-Item -LiteralPath $InstallerPath -Destination (Join-Path $inputDir 'setup.exe')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'agent-installer-inner.ps1') -Destination $inputDir
$xmlInput = [System.Security.SecurityElement]::Escape($inputDir)
$xmlOutput = [System.Security.SecurityElement]::Escape($outputDir)
$configuration = @"
<Configuration><MappedFolders>
<MappedFolder><HostFolder>$xmlInput</HostFolder><SandboxFolder>C:\fp\in</SandboxFolder><ReadOnly>true</ReadOnly></MappedFolder>
<MappedFolder><HostFolder>$xmlOutput</HostFolder><SandboxFolder>C:\fp\out</SandboxFolder><ReadOnly>false</ReadOnly></MappedFolder>
</MappedFolders><LogonCommand><Command>powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\fp\in\agent-installer-inner.ps1 -InstallerPath C:\fp\in\setup.exe -OutputPath C:\fp\out\result.json</Command></LogonCommand>
<ClipboardRedirection>Disable</ClipboardRedirection><PrinterRedirection>Disable</PrinterRedirection></Configuration>
"@
$configPath = Join-Path $stage 'agents.wsb'
[System.IO.File]::WriteAllText($configPath, $configuration)
$started = [DateTime]::UtcNow
Start-Process -FilePath $configPath -WindowStyle Hidden
try {
    while (-not (Test-Path (Join-Path $outputDir 'done'))) {
        if ([DateTime]::UtcNow -gt $started.AddMinutes($TimeoutMinutes)) { throw 'Agent installer Sandbox timed out' }
        Start-Sleep -Seconds 3
    }
    $result = Get-Content -LiteralPath (Join-Path $outputDir 'result.json') -Raw | ConvertFrom-Json
    $result | Add-Member recordedAt ([DateTime]::UtcNow.ToString('o'))
    $result | Add-Member installerSha256 ((Get-FileHash -LiteralPath $InstallerPath).Hash.ToLowerInvariant())
    New-Item -ItemType Directory -Path (Split-Path $OutputPath) -Force | Out-Null
    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutputPath -Encoding utf8
    $result | ConvertTo-Json -Depth 8
    if (-not $result.passed) { throw $result.error }
} finally { foreach ($id in Sandbox-Ids) { & wsb stop --id $id | Out-Null } }
