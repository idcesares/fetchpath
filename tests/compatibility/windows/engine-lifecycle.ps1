# Fetchpath setup with a running engine (FP-057, A12).
#
# This performs REAL per-user installs on the machine it runs on, like
# packaging-lifecycle.ps1, and leaves the machine as it found it: it refuses to
# run over an existing install, backs up %APPDATA%\app.fetchpath.desktop and
# restores it, and removes what the run created.
#
#   A. Upgrade from 0.1.0 with a persisted queue: 0.1.0 installed, its queue
#      and settings in place and its window running; the new build installed
#      over it; the queue file untouched by setup and the same jobs, history
#      and settings served by the engine.
#   B. Upgrade over a running engine mid-download, with a `fetchpath watch`
#      client attached: setup stops both before replacing fetchpath.exe, and
#      the download then finishes from its checkpoint.
#   C. Uninstall with the engine running mid-download and sign-in start on:
#      no Fetchpath process outlives it, downloads and data stay, the Run
#      value and the update hold are gone, and PATH is as before.
#
# Loopback fixtures only. Emits JSON to -OutputPath and throws on failure.

[CmdletBinding()]
param(
    [string] $OldInstallerPath,
    [string] $InstallerPath,
    [string] $UpgradeInstallerPath,
    [string] $OutputPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$bundle = Join-Path $repositoryRoot 'target\release\bundle\nsis'
if (-not $OldInstallerPath) { $OldInstallerPath = Join-Path $repositoryRoot 'work\v010\target\release\bundle\nsis\Fetchpath_0.1.0_x64-setup.exe' }
if (-not $InstallerPath) { $InstallerPath = Join-Path $bundle 'Fetchpath_0.1.1_x64-setup.exe' }
if (-not $UpgradeInstallerPath) { $UpgradeInstallerPath = Join-Path $bundle 'Fetchpath_0.1.2_x64-setup.exe' }
if (-not $OutputPath) { $OutputPath = Join-Path $repositoryRoot 'docs\development\evidence\windows\engine-lifecycle.json' }
foreach ($path in @($OldInstallerPath, $InstallerPath, $UpgradeInstallerPath)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Installer not found: $path" }
}
# An installer built before the last commit tests older code: the 28
# September re-run upgraded to a day-old 0.1.2 without noticing.
$committed = [DateTimeOffset]::FromUnixTimeSeconds([int64] (& git -C $repositoryRoot log -1 --format=%ct)).LocalDateTime
foreach ($path in @($InstallerPath, $UpgradeInstallerPath)) {
    if ((Get-Item -LiteralPath $path).LastWriteTime -lt $committed) {
        throw "$path was built before the last commit; rebuild it (CONTRIBUTING.md, Windows lifecycle checks)."
    }
}

$appData = Join-Path $env:APPDATA 'app.fetchpath.desktop'
$queuePath = Join-Path $appData 'queue-v1.json'
$holdPath = Join-Path $appData 'engine-update-hold-v1'
$installDir = Join-Path $env:LOCALAPPDATA 'Fetchpath'
$cli = Join-Path $installDir 'fetchpath.exe'
$uninstallKey = 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Fetchpath'
$manufacturerKey = 'HKCU:\SOFTWARE\Fetchpath contributors'
$webviewProfile = Join-Path $env:LOCALAPPDATA 'app.fetchpath.desktop'
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$fixtures = Join-Path $repositoryRoot 'crates\fetchpath-session\tests\fixtures'

$work = Join-Path $repositoryRoot 'work\fp057-engine-lifecycle'
if (Test-Path -LiteralPath $work) { Remove-Item -LiteralPath $work -Recurse -Force }
[System.IO.Directory]::CreateDirectory($work) | Out-Null
$downloads = Join-Path $work 'downloads'
[System.IO.Directory]::CreateDirectory($downloads) | Out-Null

$failures = [System.Collections.Generic.List[string]]::new()
function Check([bool] $Condition, [string] $Message) { if (-not $Condition) { $failures.Add($Message) } }

function Get-SignInValue {
    $property = (Get-ItemProperty -LiteralPath $runKey).PSObject.Properties['Fetchpath engine']
    if ($property) { $property.Value } else { $null }
}
function Get-UserPath { (Get-Item -LiteralPath 'HKCU:\Environment').GetValue('Path', '', 'DoNotExpandEnvironmentNames') }
function Get-Sha256([string] $Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }

# Every running process whose image is inside the install folder.
function Get-InstalledProcesses {
    @(Get-CimInstance Win32_Process | Where-Object {
        $_.ExecutablePath -and $_.ExecutablePath.StartsWith("$installDir\", [System.StringComparison]::OrdinalIgnoreCase)
    } | ForEach-Object { [ordered]@{ pid = [int] $_.ProcessId; image = Split-Path -Leaf $_.ExecutablePath; commandLine = $_.CommandLine } })
}
function Get-EngineProcess { @(Get-InstalledProcesses | Where-Object { $_.image -eq 'fetchpath.exe' -and $_.commandLine -match '"\s+engine\s*$' }) }

function Invoke-Fetchpath([string[]] $Arguments) {
    $output = & $cli @Arguments 2>&1
    [ordered]@{ exitCode = $LASTEXITCODE; lines = @($output | ForEach-Object { "$_" }) }
}
function Get-Json([string[]] $Arguments) {
    $result = Invoke-Fetchpath $Arguments
    if ($result.exitCode -ne 0) { throw "fetchpath $($Arguments -join ' ') exited $($result.exitCode): $($result.lines -join ' | ')" }
    ($result.lines | Where-Object { $_.StartsWith('{') } | Select-Object -First 1) | ConvertFrom-Json
}

function Invoke-Setup([string] $Path) {
    $started = [DateTime]::UtcNow
    $process = Start-Process -FilePath $Path -ArgumentList '/S' -PassThru
    $process.WaitForExit(300000) | Out-Null
    [ordered]@{
        installer = Split-Path -Leaf $Path
        sha256 = Get-Sha256 $Path
        exitCode = $process.ExitCode
        seconds = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 1)
    }
}

function Wait-Until([scriptblock] $Condition, [int] $Seconds, [string] $What) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        $value = & $Condition
        if ($value) { return $value }
        Start-Sleep -Milliseconds 250
    }
    throw "Timed out after $Seconds s waiting for $What."
}

function Wait-EngineGone {
    Wait-Until { @(Get-EngineProcess).Count -eq 0 } 60 'the engine to exit' | Out-Null
}

# A loopback server that sends one 24 MiB body slowly (16 KiB per 50 ms per
# connection), with a strong ETag and ranges. It counts the bytes it served
# and the ranged requests since the last /reset, so a resume after setup can be
# told from a download that started over.
$serverScript = Join-Path $work 'slow-server.mjs'
Set-Content -LiteralPath $serverScript -Encoding ascii -Value @'
import http from 'node:http';
import { createHash } from 'node:crypto';
const body = Buffer.alloc(24 * 1024 * 1024);
for (let i = 0; i < body.length; i += 1) body[i] = (i * 31) % 251;
const sha256 = createHash('sha256').update(body).digest('hex');
let served = 0;
let ranged = 0;
const server = http.createServer((req, res) => {
  if (req.url === '/stats') { res.end(JSON.stringify({ sha256, size: body.length, served, ranged })); return; }
  if (req.url === '/reset') { served = 0; ranged = 0; res.end('{}'); return; }
  const range = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range || '');
  if (range && req.headers['if-range']) ranged += 1;
  const start = range ? Number(range[1]) : 0;
  const end = range && range[2] ? Number(range[2]) : body.length - 1;
  const headers = { ETag: '"fp057"', 'Accept-Ranges': 'bytes', 'Content-Length': end - start + 1 };
  if (range) res.writeHead(206, { ...headers, 'Content-Range': `bytes ${start}-${end}/${body.length}` });
  else res.writeHead(200, headers);
  let at = start;
  const timer = setInterval(() => {
    if (at > end) { clearInterval(timer); res.end(); return; }
    const next = Math.min(at + 16 * 1024, end + 1);
    served += next - at;
    res.write(body.subarray(at, next));
    at = next;
  }, 50);
  res.on('close', () => clearInterval(timer));
});
server.listen(0, '127.0.0.1', () => console.log(server.address().port));
'@
$serverOut = Join-Path $work 'server.out'
$server = Start-Process -FilePath 'node' -ArgumentList "`"$serverScript`"" -RedirectStandardOutput $serverOut -PassThru -WindowStyle Hidden
$port = Wait-Until { if (Test-Path $serverOut) { (Get-Content -LiteralPath $serverOut -ErrorAction SilentlyContinue | Select-Object -First 1) } } 20 'the fixture server'
$fileUrl = "http://127.0.0.1:$port/file.bin"
function Get-ServerStats { Invoke-RestMethod -Uri "http://127.0.0.1:$port/stats" }
function Reset-ServerStats { Invoke-RestMethod -Uri "http://127.0.0.1:$port/reset" | Out-Null }
# The fetchpath.exe inside the upgrade installer, staged by its build.
$stagedCli = Join-Path $repositoryRoot 'apps\desktop\src-tauri\binaries\fetchpath-x86_64-pc-windows-msvc.exe'

function Wait-Midway([string] $JobId) {
    Wait-Until {
        $job = (Get-Json @('show', $JobId, '--json')).job
        if ($job.state -eq 'running' -and $job.progress.bytes_received -gt 1MB) { $job }
    } 60 "job $JobId to be under way"
}
function Wait-Completed([string] $JobId) {
    Wait-Until {
        $job = (Get-Json @('show', $JobId, '--json')).job
        if ($job.state -in @('failed', 'cancelled', 'awaiting_link')) { throw "job $JobId ended $($job.state)" }
        if ($job.state -eq 'completed') { $job }
    } 180 "job $JobId to complete"
}

$observation = [ordered]@{
    task = 'FP-057'
    recordedAt = [DateTime]::UtcNow.ToString('o')
    machine = [ordered]@{ os = (Get-CimInstance Win32_OperatingSystem).Caption; build = [Environment]::OSVersion.Version.ToString() }
}

if (Test-Path -LiteralPath $uninstallKey) { throw 'Fetchpath is already installed; this run will not uninstall somebody''s copy.' }
$appDataExisted = Test-Path -LiteralPath $appData
$manufacturerExisted = Test-Path -LiteralPath $manufacturerKey
$webviewExisted = Test-Path -LiteralPath $webviewProfile
$backup = Join-Path $work 'preexisting-appdata'
if ($appDataExisted) {
    Copy-Item -LiteralPath $appData -Destination $backup -Recurse
    Remove-Item -LiteralPath $appData -Recurse -Force
}
$pathBefore = Get-UserPath
$installed = $false
$desktop = $null
$watch = $null
$stuck = $null
$spared = $null

try {
    # ------------------------------------------------- A. from 0.1.0 -------
    $a = [ordered]@{}
    $a.install010 = Invoke-Setup $OldInstallerPath
    $installed = $true
    Check ($a.install010.exitCode -eq 0) '0.1.0 setup failed.'

    # A queue and settings as 0.1.0 wrote them (the FP-048 fixtures), which
    # the 0.1.0 window then loads, works on and saves while it runs.
    [System.IO.Directory]::CreateDirectory($appData) | Out-Null
    $jsonDir = $downloads.Replace('\', '\\')
    $queueText = [System.IO.File]::ReadAllText((Join-Path $fixtures 'queue-0.1.0.json')).Replace('{DIR}', $jsonDir)
    [System.IO.File]::WriteAllText($queuePath, $queueText, [System.Text.UTF8Encoding]::new($false))
    Copy-Item -LiteralPath (Join-Path $fixtures 'settings-0.1.0.json') -Destination (Join-Path $appData 'settings-v1.json')
    $desktop = Start-Process -FilePath (Join-Path $installDir 'fetchpath-desktop.exe') -PassThru
    Start-Sleep -Seconds 10
    $desktop.Refresh()
    $a.window010RunningDuringUpgrade = -not $desktop.HasExited

    # The window saves by replacing the file; take it once two reads agree.
    $queueHashBefore = Wait-Until {
        try {
            $first = Get-Sha256 $queuePath
            Start-Sleep -Milliseconds 500
            if ((Get-Sha256 $queuePath) -eq $first) { $first }
        } catch { $null }
    } 30 'the 0.1.0 queue to settle'
    $before = Get-Content -LiteralPath $queuePath -Raw | ConvertFrom-Json
    $a.jobsBefore = @($before.records).Count
    $completedBefore = @($before.records | Where-Object { $_.view.state -eq 'completed' } |
        ForEach-Object { [ordered]@{ id = $_.id; destination = $_.view.destination; sha256 = $_.view.observedSha256 } })
    $settingsHashBefore = Get-Sha256 (Join-Path $appData 'settings-v1.json')

    $a.upgrade = Invoke-Setup $InstallerPath
    Check ($a.upgrade.exitCode -eq 0) 'Upgrade from 0.1.0 failed.'
    $desktop.Refresh()
    $a.window010StoppedBySetup = $desktop.HasExited
    $a.queueFileUntouchedBySetup = ((Get-Sha256 $queuePath) -eq $queueHashBefore)
    $a.settingsFileUntouchedBySetup = ((Get-Sha256 (Join-Path $appData 'settings-v1.json')) -eq $settingsHashBefore)
    Check $a.queueFileUntouchedBySetup 'Setup changed the queue file.'
    Check $a.settingsFileUntouchedBySetup 'Setup changed the settings file.'

    $jobs = @((Get-Json @('ls', '--all', '--json')).jobs)
    $byId = @{}; foreach ($job in $jobs) { $byId[$job.job_id] = $job }
    $a.jobsServedByEngine = $jobs.Count
    Check ($jobs.Count -eq $a.jobsBefore) "The engine serves $($jobs.Count) jobs; 0.1.0 had saved $($a.jobsBefore)."
    $missing = @($before.records | Where-Object { -not $byId.ContainsKey($_.id) } | ForEach-Object { $_.id })
    Check ($missing.Count -eq 0) "Jobs lost in the upgrade: $($missing -join ', ')"
    $keptCompleted = @($completedBefore | Where-Object {
        $job = $byId[$_.id]
        $job -and $job.state -eq 'completed' -and $job.observed_sha256 -eq $_.sha256
    })
    $a.completedKept = "$($keptCompleted.Count) of $($completedBefore.Count)"
    Check ($keptCompleted.Count -eq $completedBefore.Count) 'A completed job changed in the upgrade.'
    $history = @((Get-Json @('history', '--json')).jobs | ForEach-Object { $_.job_id })
    $a.historyKeepsCompleted = @($completedBefore | Where-Object { $history -contains $_.id }).Count -eq $completedBefore.Count
    Check $a.historyKeepsCompleted 'History lost a completed download.'
    $settings = (Get-Json @('settings', '--json')).view.settings
    $a.settingsKept = ($settings.theme -eq 'dark') -and ($settings.max_active_downloads -eq 5) -and
        ($settings.default_destination_dir -eq 'C:\Downloads\Fetchpath') -and ($settings.power_mode -eq $true)
    Check $a.settingsKept 'Settings changed in the upgrade.'
    $observation.fromVersion010 = $a

    # Clear the fixture jobs so the rest of the run watches its own.
    Invoke-Fetchpath @('engine', 'stop') | Out-Null
    Wait-EngineGone

    # ------------------------------ B. over a running engine, mid-download ---
    $b = [ordered]@{}
    # One byte appended to the installed CLI (still a valid image), so that
    # "replaced" can be told apart from "left alone" by its hash.
    $pristine = Get-Sha256 $cli
    [System.IO.File]::AppendAllText($cli, 'x')
    $marked = Get-Sha256 $cli
    $b.markedBeforeUpgrade = ($marked -ne $pristine)

    $target = Join-Path $downloads 'over-upgrade.bin'
    $added = Get-Json @('add', $fileUrl, '--to', $target, '--json')
    $jobId = $added.job.job_id
    Wait-Midway $jobId | Out-Null
    $engineBefore = @(Get-EngineProcess)
    $b.enginePidBefore = if ($engineBefore.Count) { $engineBefore[0].pid } else { $null }
    Check ($engineBefore.Count -eq 1) 'No engine was running before the upgrade.'
    $watchOut = Join-Path $work 'watch.out'
    $watch = Start-Process -FilePath $cli -ArgumentList @('watch', $jobId) -RedirectStandardOutput $watchOut `
        -RedirectStandardError (Join-Path $work 'watch.err') -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 2
    $midway = (Get-Json @('show', $jobId, '--json')).job
    $b.bytesBeforeUpgrade = $midway.progress.bytes_received
    $watch.Refresh()
    $b.stillDownloadingWhenSetupStarted = ($midway.state -eq 'running') -and ($midway.progress.bytes_received -lt 20MB)
    $b.watchAliveWhenSetupStarted = -not $watch.HasExited
    Check ($b.stillDownloadingWhenSetupStarted -and $b.watchAliveWhenSetupStarted) 'The download or the watch client had ended before setup started.'

    $b.upgrade = Invoke-Setup $UpgradeInstallerPath
    Check ($b.upgrade.exitCode -eq 0) 'Upgrade over a running engine failed.'
    # Well under the hook's 20-second wait: the engine stopped cleanly and
    # nothing had to be ended.
    Check ($b.upgrade.seconds -lt 20) 'Setup waited out the hook instead of stopping the engine cleanly.'
    $b.displayVersion = (Get-ItemProperty -LiteralPath $uninstallKey).DisplayVersion
    Check ($b.displayVersion -eq '0.1.2') "Installed version is $($b.displayVersion)."
    # The installed copy must now be the one this installer carried, not the
    # marked one it found running.
    $b.cliSha256 = [ordered]@{ marked = $marked; carried = Get-Sha256 $stagedCli; afterUpgrade = Get-Sha256 $cli }
    $b.cliReplaced = ($b.cliSha256.afterUpgrade -eq $b.cliSha256.carried) -and ($b.cliSha256.afterUpgrade -ne $marked)
    Check $b.cliReplaced 'fetchpath.exe was not replaced.'
    $b.oldEngineGone = -not (Get-Process -Id $b.enginePidBefore -ErrorAction SilentlyContinue)
    Check $b.oldEngineGone 'The old engine outlived the upgrade.'
    $watch.Refresh()
    $b.watchClientEnded = $watch.HasExited
    $b.watchClientExitCode = if ($watch.HasExited) { $watch.ExitCode } else { $null }
    $b.watchClientSaid = @(Get-Content -LiteralPath (Join-Path $work 'watch.err') -ErrorAction SilentlyContinue) -join ' '
    $b.holdLifted = -not (Test-Path -LiteralPath $holdPath)
    Check $b.holdLifted 'Setup left the update hold behind.'
    Check (($b.watchClientExitCode -eq 1) -and ($b.watchClientSaid -match 'being updated')) 'The watch client did not stop with the update explanation.'
    $b.processesRightAfterSetup = @(Get-InstalledProcesses)
    Reset-ServerStats

    $finished = Wait-Completed $jobId
    $stats = Get-ServerStats
    $b.completedAfterUpgrade = $true
    $b.fileMatches = ((Get-Sha256 $target) -eq $stats.sha256) -and ($finished.observed_sha256 -eq $stats.sha256)
    $b.bytesServedAfterUpgrade = $stats.served
    $b.rangedRequestsAfterUpgrade = $stats.ranged
    Check $b.fileMatches 'The download finished with different bytes.'
    Check (($stats.ranged -gt 0) -and ($stats.served -lt $stats.size)) 'The download started over instead of resuming.'
    $observation.overRunningEngine = $b

    # ---------------------------- C. uninstall with the engine running -------
    $c = [ordered]@{}
    $set = Invoke-Fetchpath @('settings', 'start-engine-at-sign-in', 'on')
    $c.signInValueSet = Get-SignInValue
    Check ([bool] $c.signInValueSet) "Sign-in start was not written ($($set.lines -join ' '))."
    $second = Join-Path $downloads 'during-uninstall.bin'
    $jobId2 = (Get-Json @('add', $fileUrl, '--to', $second, '--json')).job.job_id
    Wait-Midway $jobId2 | Out-Null
    $c.enginePidBefore = @(Get-EngineProcess)[0].pid
    # A command that holds fetchpath.exe without the engine: `batch -` waiting
    # for input that never comes. Only the hook's last resort ends it, and it
    # must spare the same command run from a copy outside the install.
    function Start-Stuck([string] $Exe) {
        $info = [System.Diagnostics.ProcessStartInfo]::new($Exe, 'batch -')
        $info.UseShellExecute = $false
        $info.RedirectStandardInput = $true
        $info.RedirectStandardOutput = $true
        $info.RedirectStandardError = $true
        $info.CreateNoWindow = $true
        [System.Diagnostics.Process]::Start($info)
    }
    $elsewhere = Join-Path $work 'elsewhere'
    [System.IO.Directory]::CreateDirectory($elsewhere) | Out-Null
    Copy-Item -LiteralPath $cli -Destination (Join-Path $elsewhere 'fetchpath.exe')
    $stuck = Start-Stuck $cli
    $spared = Start-Stuck (Join-Path $elsewhere 'fetchpath.exe')
    Start-Sleep -Seconds 1
    Check ((-not $stuck.HasExited) -and (-not $spared.HasExited)) 'The stuck commands ended by themselves.'

    $started = [DateTime]::UtcNow
    $uninstall = Start-Process -FilePath (Join-Path $installDir 'uninstall.exe') -ArgumentList '/S' -PassThru
    $uninstall.WaitForExit(120000) | Out-Null
    Wait-Until { -not (Test-Path -LiteralPath $installDir) -and -not (Test-Path -LiteralPath $uninstallKey) } 120 'uninstall' | Out-Null
    $c.seconds = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 1)
    $installed = $false
    $c.engineGone = -not (Get-Process -Id $c.enginePidBefore -ErrorAction SilentlyContinue)
    Check $c.engineGone 'The engine outlived the uninstall.'
    $c.stuckCommandInInstallEnded = $stuck.HasExited
    $c.sameCommandElsewhereSpared = -not $spared.HasExited
    Check $c.stuckCommandInInstallEnded 'A command stuck in the install folder outlived the uninstall.'
    Check $c.sameCommandElsewhereSpared 'The uninstaller ended a fetchpath.exe outside its install.'
    Check ($c.seconds -ge 20) 'The stuck command was ended before the hook had waited for it.'
    if (-not $spared.HasExited) { $spared.Kill() }
    Start-Sleep -Seconds 15
    $c.fetchpathProcessesAfter15s = @(Get-Process -Name 'fetchpath', 'fetchpath-desktop' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith("$installDir\", [System.StringComparison]::OrdinalIgnoreCase) } |
        ForEach-Object { $_.Id })
    Check ($c.fetchpathProcessesAfter15s.Count -eq 0) 'A Fetchpath process is running after uninstall.'
    $c.completedDownloadKept = (Test-Path -LiteralPath $target) -and ((Get-Sha256 $target) -eq (Get-ServerStats).sha256)
    Check $c.completedDownloadKept 'Uninstall touched a finished download.'
    $c.queueKept = Test-Path -LiteralPath $queuePath
    Check $c.queueKept 'Uninstall removed the queue.'
    $saved = Get-Content -LiteralPath $queuePath -Raw | ConvertFrom-Json
    $c.interruptedJobSaved = @($saved.records | Where-Object { $_.id -eq $jobId2 } | ForEach-Object { [ordered]@{ state = $_.view.state; bytesReceived = $_.view.bytesReceived } })
    Check (($c.interruptedJobSaved.Count -eq 1) -and ($c.interruptedJobSaved[0].bytesReceived -gt 0)) 'The interrupted download was not saved with its progress.'
    $c.signInValueRemoved = -not (Get-SignInValue)
    Check $c.signInValueRemoved 'Uninstall left the sign-in start behind.'
    $c.holdLifted = -not (Test-Path -LiteralPath $holdPath)
    Check $c.holdLifted 'Uninstall left the update hold behind.'
    $c.userPathRestored = ((Get-UserPath) -eq $pathBefore)
    Check $c.userPathRestored 'The user PATH differs from before the run.'
    $observation.uninstallWithEngineRunning = $c
} catch {
    $observation.abortedWith = $_.Exception.Message
    $failures.Add("Run aborted: $($_.Exception.Message) at line $($_.InvocationInfo.ScriptLineNumber)")
} finally {
    foreach ($candidate in @($desktop, $watch, $stuck, $spared)) {
        if ($candidate) { $candidate.Refresh(); if (-not $candidate.HasExited) { Stop-Process -Id $candidate.Id -Force } }
    }
    $cleanup = [ordered]@{}
    if ($installed -and (Test-Path -LiteralPath (Join-Path $installDir 'uninstall.exe'))) {
        Start-Process -FilePath (Join-Path $installDir 'uninstall.exe') -ArgumentList '/S' -Wait
        $deadline = [DateTime]::UtcNow.AddSeconds(60)
        while ((Test-Path -LiteralPath $installDir) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Seconds 1 }
        $cleanup.forcedUninstall = $true
    }
    foreach ($stray in Get-InstalledProcesses) { Stop-Process -Id $stray.pid -Force -ErrorAction SilentlyContinue }
    if ($server -and -not $server.HasExited) { Stop-Process -Id $server.Id -Force }
    if (Test-Path -LiteralPath $appData) { Remove-Item -LiteralPath $appData -Recurse -Force }
    if ($appDataExisted) { Copy-Item -LiteralPath $backup -Destination $appData -Recurse; $cleanup.appDataRestored = $true }
    if (-not $manufacturerExisted -and (Test-Path -LiteralPath $manufacturerKey)) { Remove-Item -LiteralPath $manufacturerKey -Recurse -Force }
    if (-not $webviewExisted -and (Test-Path -LiteralPath $webviewProfile)) { Remove-Item -LiteralPath $webviewProfile -Recurse -Force }
    $cleanup.installPresentAtExit = Test-Path -LiteralPath $uninstallKey
    $cleanup.userPathAsBefore = ((Get-UserPath) -eq $pathBefore)
    $observation.machineRestored = $cleanup
}

$observation.failures = $failures
$observation.passed = ($failures.Count -eq 0)
[System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($OutputPath)) | Out-Null
$json = $observation | ConvertTo-Json -Depth 8
[System.IO.File]::WriteAllText($OutputPath, $json, [System.Text.UTF8Encoding]::new($false))
$json
if ($failures.Count -gt 0) { throw "Engine lifecycle checks failed:`n - $($failures -join "`n - ")" }
