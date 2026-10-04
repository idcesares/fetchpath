# FP-103 G2: actual guest logoff/logon, never the owner's Windows session.
[CmdletBinding()]
param([string]$InstallerPath,[string]$OutputPath)
$ErrorActionPreference='Stop'
$repo=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
if (-not $InstallerPath) { $InstallerPath=Join-Path $repo 'target/release/bundle/nsis/Fetchpath_0.2.0_x64-setup.exe' }
if (-not $OutputPath) { $OutputPath=Join-Path $repo 'docs/development/evidence/windows/sign-in-lifecycle.json' }
function Sandbox-Ids { return ,@((& wsb list --raw | Out-String | ConvertFrom-Json).WindowsSandboxEnvironments | ForEach-Object { $_.Id }) }
if ((Sandbox-Ids).Count -ne 0) { throw 'Close the existing Sandbox first' }
if (-not (Test-Path -LiteralPath $InstallerPath)) { throw 'Build the current release installer first' }
$stage=Join-Path $repo ('work/fp103-signin-'+[Guid]::NewGuid().ToString('N'))
$in=Join-Path $stage 'in'; $out=Join-Path $stage 'out'
New-Item -ItemType Directory -Path $in,$out | Out-Null
Copy-Item -LiteralPath $InstallerPath -Destination (Join-Path $in 'setup.exe')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'sign-in-lifecycle-inner.ps1') -Destination $in
$xmlIn=[Security.SecurityElement]::Escape($in); $xmlOut=[Security.SecurityElement]::Escape($out)
$config="<Configuration><MappedFolders><MappedFolder><HostFolder>$xmlIn</HostFolder><SandboxFolder>C:\fp\in</SandboxFolder><ReadOnly>true</ReadOnly></MappedFolder><MappedFolder><HostFolder>$xmlOut</HostFolder><SandboxFolder>C:\fp\out</SandboxFolder><ReadOnly>false</ReadOnly></MappedFolder></MappedFolders><LogonCommand><Command>powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\fp\in\sign-in-lifecycle-inner.ps1</Command></LogonCommand><ClipboardRedirection>Disable</ClipboardRedirection><PrinterRedirection>Disable</PrinterRedirection></Configuration>"
$configPath=Join-Path $stage 'signin.wsb'; [IO.File]::WriteAllText($configPath,$config)
function Wait-File([string]$Name,[int]$Seconds=180) {
    $deadline=[DateTime]::UtcNow.AddSeconds($Seconds)
    while (-not (Test-Path (Join-Path $out $Name))) {
        if (Test-Path (Join-Path $out 'error.json')) { throw (Get-Content (Join-Path $out 'error.json') -Raw) }
        if ([DateTime]::UtcNow -ge $deadline) { throw "Timed out waiting for $Name" }
        Start-Sleep -Milliseconds 500
    }
}
function Execute-Phase([string]$Phase,[string]$As='ExistingLogin') {
    & wsb exec --id $sandbox --run-as $As --command "powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\fp\in\sign-in-lifecycle-inner.ps1 -Phase $Phase"
    if ($LASTEXITCODE -ne 0) { throw "Guest phase failed: $Phase" }
}
Start-Process -FilePath $configPath -WindowStyle Hidden
try {
    Wait-File 'setup.json' 660
    $sandbox=(Sandbox-Ids)[0]
    Execute-Phase 'ServeLaunch' 'System'
    Wait-File 'server-ready' 30
    Execute-Phase 'Before'
    Wait-File 'before.json' 30
    & wsb exec --id $sandbox --run-as ExistingLogin --command 'cmd.exe /c shutdown.exe /l'
    if ($LASTEXITCODE -ne 0) { throw 'Guest logoff failed' }
    $logoffDeadline=[DateTime]::UtcNow.AddSeconds(90)
    do {
        Execute-Phase 'LogoffProbe' 'System'
        if (Test-Path (Join-Path $out 'logoff-ready')) { break }
        Start-Sleep -Seconds 2
    } while ([DateTime]::UtcNow -lt $logoffDeadline)
    if (-not (Test-Path (Join-Path $out 'logoff-ready'))) { throw 'Guest logoff did not finish' }
    Start-Process wsb.exe -ArgumentList @('connect','--id',$sandbox) -WindowStyle Hidden
    $loginDeadline=[DateTime]::UtcNow.AddSeconds(90)
    do {
        # A failed process launch can still return a successful wsb CLI exit.
        & wsb exec --id $sandbox --run-as ExistingLogin --command 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\fp\in\sign-in-lifecycle-inner.ps1 -Phase LoginProbe' | Out-Null
        if ((Test-Path (Join-Path $out 'login-ready.txt')) -and (Get-Item (Join-Path $out 'login-ready.txt')).Length -gt 0) { break }
        Start-Sleep -Seconds 3
    } while ([DateTime]::UtcNow -lt $loginDeadline)
    if (-not (Test-Path (Join-Path $out 'login-ready.txt')) -or (Get-Item (Join-Path $out 'login-ready.txt')).Length -eq 0) { throw 'Guest sign-in did not become available' }
    Execute-Phase 'After'
    Wait-File 'result.json'
    $result=Get-Content (Join-Path $out 'result.json') -Raw | ConvertFrom-Json
    $result | Add-Member installerSha256 ((Get-FileHash -LiteralPath $InstallerPath).Hash.ToLowerInvariant())
    $result | Add-Member recordedAt ([DateTime]::UtcNow.ToString('o'))
    New-Item -ItemType Directory -Path (Split-Path $OutputPath) -Force | Out-Null
    $result | ConvertTo-Json | Set-Content -LiteralPath $OutputPath -Encoding utf8
    $result | ConvertTo-Json
    if (-not $result.passed) { throw 'Guest lifecycle failed' }
} finally { foreach ($id in Sandbox-Ids) { & wsb stop --id $id | Out-Null } }
