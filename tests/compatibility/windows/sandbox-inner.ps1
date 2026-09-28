# Fetchpath clean-machine lifecycle, the half that runs INSIDE Windows Sandbox
# (FP-043, A12; also FP-044's `fetchpath --help` check; FP-069 adds the engine).
#
# sandbox-lifecycle.ps1 on the host starts a fresh Sandbox, maps the installer
# and these scripts read-only and an output folder writable, and runs this as
# the logon command. The Sandbox is a pristine Windows image that is thrown away
# afterwards, so unlike packaging-lifecycle.ps1 this script does not back up or
# restore anything.
#
# Sequence: preflight -> silent install -> `fetchpath --help`, a CLI download
# and a queued download through the engine, from a terminal Explorer starts (a
# genuinely new terminal) -> a download in the desktop app -> silent uninstall
# with the engine still running -> residue.
#
# Writes JSON to -OutputPath, then a `done` marker beside it, whatever happens.

[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $InstallerPath,
    [Parameter(Mandatory)] [string] $OutputPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$outputDirectory = [System.IO.Path]::GetDirectoryName($OutputPath)
$doneMarker = Join-Path $outputDirectory 'done'
$workDirectory = Join-Path $env:TEMP 'fp043'
[System.IO.Directory]::CreateDirectory($workDirectory) | Out-Null

$progressLog = Join-Path $outputDirectory 'progress.txt'
function Write-Progress-Line([string] $Text) {
    [System.IO.File]::AppendAllText($progressLog, "$([DateTime]::UtcNow.ToString('HH:mm:ss')) $Text`r`n")
}

$failures = [System.Collections.Generic.List[string]]::new()
function Assert-True([bool] $Condition, [string] $Message) {
    if (-not $Condition) { $failures.Add($Message) }
}

$observation = [ordered]@{}
$observation.environment = 'Windows Sandbox'
$observation.osBuild = [string] [System.Environment]::OSVersion.Version
$observation.machineArchitecture = $env:PROCESSOR_ARCHITECTURE
$observation.installerSha256 = (Get-FileHash -LiteralPath $InstallerPath -Algorithm SHA256).Hash.ToLowerInvariant()

$fixture = $null
$appProcess = $null
$installDirectory = Join-Path $env:LOCALAPPDATA 'Fetchpath'
$hostName = 'com.fetchpath.browser'
$nativeHostKeys = @(
    "HKCU:\Software\Google\Chrome\NativeMessagingHosts\$hostName",
    "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\$hostName",
    "HKCU:\Software\Mozilla\NativeMessagingHosts\$hostName"
)

function Get-UserPath {
    return [string] (Get-ItemProperty -LiteralPath 'HKCU:\Environment' -Name Path -ErrorAction SilentlyContinue).Path
}

function Test-PathListContains([string] $PathList, [string] $Directory) {
    foreach ($entry in ($PathList -split ';')) {
        if ($entry.TrimEnd('\') -ieq $Directory.TrimEnd('\')) { return $true }
    }
    return $false
}

function Get-FetchpathProcesses {
    return , @(Get-CimInstance Win32_Process | Where-Object { $_.Name -like 'fetchpath*.exe' } |
        ForEach-Object { [ordered]@{ image = $_.Name; commandLine = $_.CommandLine } })
}

function Get-FetchpathUninstallEntries {
    $root = 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall'
    if (-not (Test-Path -LiteralPath $root)) { return , @() }
    $entries = foreach ($key in Get-ChildItem -LiteralPath $root) {
        $properties = Get-ItemProperty -LiteralPath $key.PSPath
        $name = $properties.PSObject.Properties['DisplayName']
        if ($name -and $name.Value -like '*Fetchpath*') {
            [ordered]@{
                displayName = $name.Value
                displayVersion = $properties.PSObject.Properties['DisplayVersion'].Value
            }
        }
    }
    return , @($entries)
}

# Explorer opens the script the way a double-click would, so the command runs in
# a process that inherits Explorer's environment, refreshed by the installer's
# WM_SETTINGCHANGE broadcast. That is exactly what "a new terminal after
# install" means to a user, and it is why this does not simply read the
# registry PATH into a child process.
function Invoke-FromNewTerminal([string] $Name, [string] $CommandLine, [int] $TimeoutSeconds = 60) {
    $stdout = Join-Path $workDirectory "$Name.out.txt"
    $exitFile = Join-Path $workDirectory "$Name.exit.txt"
    $script = Join-Path $workDirectory "$Name.cmd"
    foreach ($file in @($stdout, $exitFile)) { if (Test-Path -LiteralPath $file) { Remove-Item -LiteralPath $file } }
    $content = "@echo off`r`n$CommandLine > `"$stdout`" 2>&1`r`necho %ERRORLEVEL% > `"$exitFile`"`r`nexit`r`n"
    [System.IO.File]::WriteAllText($script, $content, [System.Text.Encoding]::ASCII)
    Start-Process -FilePath 'explorer.exe' -ArgumentList "`"$script`""
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    while (-not (Test-Path -LiteralPath $exitFile) -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 250
    }
    Start-Sleep -Milliseconds 250
    $exitCode = if (Test-Path -LiteralPath $exitFile) {
        [int] ((Get-Content -LiteralPath $exitFile -Raw).Trim())
    } else { $null }
    # Read as plain UTF-8 text: Get-Content in Windows PowerShell 5.1 decodes
    # as ANSI and attaches provider properties that ConvertTo-Json serializes.
    $output = if (Test-Path -LiteralPath $stdout) { [System.IO.File]::ReadAllText($stdout, [System.Text.Encoding]::UTF8) } else { $null }
    return [ordered]@{ commandLine = $CommandLine; exitCode = $exitCode; output = $output }
}

try {
    . (Join-Path $PSScriptRoot 'uia-common.ps1')
    $sacKey = 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy'

    # ----------------------------------------------------------- preflight ---
    $webview2 = $null
    foreach ($key in @(
            'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
            'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}')) {
        if (Test-Path -LiteralPath $key) { $webview2 = (Get-ItemProperty -LiteralPath $key).pv; break }
    }
    # The Sandbox image ships WebView2 as a system component without the
    # EdgeUpdate registration, so fall back to the runtime's own folder.
    if (-not $webview2) {
        $runtimeRoot = Join-Path ${env:ProgramFiles(x86)} 'Microsoft\EdgeWebView\Application'
        if (Test-Path -LiteralPath $runtimeRoot) {
            $webview2 = Get-ChildItem -LiteralPath $runtimeRoot -Directory | Where-Object { $_.Name -match '^\d+\.' } |
                Sort-Object { [version] $_.Name } | Select-Object -Last 1 -ExpandProperty Name
        }
    }
    $observation.preflight = [ordered]@{
        # The point of FP-044: a fresh image has no Visual C++ redistributable.
        vcruntime140Present = (Test-Path -LiteralPath (Join-Path $env:SystemRoot 'System32\vcruntime140.dll'))
        webview2RuntimeVersion = $webview2
        fetchpathOnPathBefore = [bool] (Get-Command fetchpath -ErrorAction SilentlyContinue)
        installDirectoryExistedBefore = (Test-Path -LiteralPath $installDirectory)
        uninstallEntriesBefore = (Get-FetchpathUninstallEntries).Count
        userPathBefore = Get-UserPath
        # Smart App Control (the VerifiedAndReputable policy) blocks an
        # unsigned program outright, not with the SmartScreen warning.
        smartAppControlState = [int] (Get-ItemProperty -LiteralPath $sacKey -Name VerifiedAndReputablePolicyState -ErrorAction SilentlyContinue).VerifiedAndReputablePolicyState
    }
    Assert-True (-not $observation.preflight.installDirectoryExistedBefore) 'The Sandbox image already had a Fetchpath install directory.'
    # Recent Sandbox images enforce it. Builds are unsigned by decision, so the
    # clean machine is one with it off, as on the development host; the
    # limitation is stated for users.
    if ($observation.preflight.smartAppControlState -ne 0) {
        Write-Progress-Line "smart app control state $($observation.preflight.smartAppControlState); turning it off"
        Set-ItemProperty -LiteralPath $sacKey -Name VerifiedAndReputablePolicyState -Value 0 -Type DWord
        $refresh = Start-Process -FilePath 'CiTool.exe' -ArgumentList '--refresh', '--json' -PassThru -WindowStyle Hidden
        if (-not $refresh.WaitForExit(60000)) { Stop-Process -Id $refresh.Id -Force; Write-Progress-Line 'CiTool refresh timed out' }
        $observation.preflight.smartAppControlTurnedOff = $true
    }

    # ------------------------------------------------------------- install ---
    Write-Progress-Line 'install'
    $started = [DateTime]::UtcNow
    # Run a local copy, as a person runs the file they downloaded, rather than
    # one on the read-only mapped folder.
    $localInstaller = Join-Path $workDirectory ([System.IO.Path]::GetFileName($InstallerPath))
    Copy-Item -LiteralPath $InstallerPath -Destination $localInstaller -Force
    $installer = Start-Process -FilePath $localInstaller -ArgumentList '/S' -PassThru -Wait
    $observation.install = [ordered]@{
        exitCode = $installer.ExitCode
        elapsedSeconds = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 1)
        uninstallEntries = Get-FetchpathUninstallEntries
        desktopExecutable = (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath-desktop.exe'))
        cliExecutable = (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath.exe'))
        browserHostExecutable = (Test-Path -LiteralPath (Join-Path $installDirectory "fetchpath-browser-host.exe"))
        installDirectoryOnUserPath = (Test-PathListContains (Get-UserPath) $installDirectory)
        nativeHostKeysRegistered = @($nativeHostKeys | Where-Object { Test-Path -LiteralPath $_ }).Count
    }
    Assert-True ($observation.install.exitCode -eq 0) "The installer exited with code $($observation.install.exitCode)."
    Assert-True (@($observation.install.uninstallEntries).Count -eq 1) 'Expected exactly one per-user uninstall entry.'
    Assert-True $observation.install.desktopExecutable 'fetchpath-desktop.exe was not installed.'
    Assert-True $observation.install.cliExecutable 'fetchpath.exe was not installed.'
    Assert-True $observation.install.installDirectoryOnUserPath 'The install directory was not added to the user PATH.'
    Assert-True ($observation.install.nativeHostKeysRegistered -eq 3) `
        "Expected the browser host registered for Chrome, Edge and Firefox; found $($observation.install.nativeHostKeysRegistered)."

    # ------------------------------------------------ CLI in a new terminal ---
    Write-Progress-Line "installed with exit code $($observation.install.exitCode); CLI"
    $fixture = Start-FixtureServer -Size (256 * 1024)
    $observation.cli = [ordered]@{}
    $help = Invoke-FromNewTerminal 'help' 'fetchpath --help'
    $observation.cli.help = $help
    Assert-True ($help.exitCode -eq 0) "``fetchpath --help`` from a new terminal exited with $($help.exitCode)."
    Assert-True ($help.output -and $help.output -match 'download') '`fetchpath --help` printed no usage text.'
    $version = Invoke-FromNewTerminal 'version' 'fetchpath --version'
    $observation.cli.version = $version
    Assert-True ($version.exitCode -eq 0 -and $version.output -match '0\.1\.0') "``fetchpath --version`` did not report 0.1.0: $($version.output)"

    $cliDestination = Join-Path $workDirectory 'cli-download.bin'
    $cliDownload = Invoke-FromNewTerminal 'download' "fetchpath download $($fixture.BaseUrl)/cli.bin `"$cliDestination`" --json" 120
    $observation.cli.download = $cliDownload
    $observation.cli.downloadedBytes = if (Test-Path -LiteralPath $cliDestination) { (Get-Item -LiteralPath $cliDestination).Length } else { $null }
    Assert-True ($cliDownload.exitCode -eq 0) "The CLI download exited with $($cliDownload.exitCode): $($cliDownload.output)"
    Assert-True ($observation.cli.downloadedBytes -eq 262144) "The CLI saved $($observation.cli.downloadedBytes) bytes, expected 262144."

    # The queue belongs to the engine, which the command starts on demand.
    $queueDirectory = Join-Path $workDirectory 'queued'
    [System.IO.Directory]::CreateDirectory($queueDirectory) | Out-Null
    $queued = Invoke-FromNewTerminal 'queue' "fetchpath add $($fixture.BaseUrl)/queued.bin --to `"$queueDirectory`" --wait" 120
    $observation.cli.queuedDownload = $queued
    $queuedFile = Join-Path $queueDirectory 'queued.bin'
    $observation.cli.queuedBytes = if (Test-Path -LiteralPath $queuedFile) { (Get-Item -LiteralPath $queuedFile).Length } else { $null }
    Assert-True ($queued.exitCode -eq 0) "``fetchpath add --wait`` exited with $($queued.exitCode): $($queued.output)"
    Assert-True ($observation.cli.queuedBytes -eq 262144) "The engine saved $($observation.cli.queuedBytes) bytes, expected 262144."
    $status = Invoke-FromNewTerminal 'engine-status' 'fetchpath engine status'
    $observation.cli.engineStatus = $status
    Assert-True ($status.exitCode -eq 0) "``fetchpath engine status`` exited with $($status.exitCode): $($status.output)"

    # --------------------------------------------------- desktop download ---
    Write-Progress-Line 'desktop download'
    $appProcess = Start-Process -FilePath (Join-Path $installDirectory 'fetchpath-desktop.exe') -PassThru
    $renderer = Get-RendererRoot $appProcess 90
    $appDestination = Join-Path $workDirectory 'app-download.bin'
    Set-Field $renderer 'url' "$($fixture.BaseUrl)/app.bin"
    Start-Sleep -Milliseconds 600
    Set-Field $renderer 'destination' $appDestination
    Start-Sleep -Milliseconds 300
    Invoke-Control $renderer 'start-download' $appProcess
    $observation.app = [ordered]@{ launched = $true }
    $observation.app.status = Wait-ForStatus $renderer @('Complete', 'Needs attention', 'Link needed') 90
    $published = Wait-ForCondition {
        if (Test-Path -LiteralPath $appDestination -PathType Leaf) { Get-Item -LiteralPath $appDestination } else { $null }
    } 20 'the desktop download to be published'
    $observation.app.publishedBytes = $published.Length
    $observation.app.publishedSha256 = (Get-FileHash -LiteralPath $appDestination -Algorithm SHA256).Hash.ToLowerInvariant()
    $observation.app.matchesCliDownload = ($observation.app.publishedSha256 -eq (Get-FileHash -LiteralPath $cliDestination -Algorithm SHA256).Hash.ToLowerInvariant())
    Assert-True ($observation.app.status -eq 'Complete') "The desktop download ended in '$($observation.app.status)'."
    Assert-True ($observation.app.publishedBytes -eq 262144) "The app saved $($observation.app.publishedBytes) bytes, expected 262144."
    Assert-True $observation.app.matchesCliDownload 'The desktop and CLI downloads of the same fixture differ.'
    $observation.app.fixture = Get-FixtureDiagnostics $fixture

    Stop-Process -Id $appProcess.Id -Force
    $appProcess = $null
    Start-Sleep -Seconds 1

    # ----------------------------------------------------------- uninstall ---
    Write-Progress-Line 'uninstall'
    # Closing the window leaves the engine running for its idle grace, so
    # uninstall meets a live engine, as a person uninstalling right away would.
    $processesBefore = Get-FetchpathProcesses
    $engineRunning = @($processesBefore | Where-Object { $_.image -eq 'fetchpath.exe' -and $_.commandLine -match '\sengine\s*$' }).Count -gt 0
    Assert-True $engineRunning 'No engine was running when uninstall started.'
    $started = [DateTime]::UtcNow
    $uninstaller = Start-Process -FilePath (Join-Path $installDirectory 'uninstall.exe') -ArgumentList '/S' -PassThru
    $uninstaller.WaitForExit(120000) | Out-Null
    $deadline = [DateTime]::UtcNow.AddSeconds(90)
    do {
        Start-Sleep -Seconds 2
        $settled = (-not (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath-desktop.exe'))) -and
            ((Get-FetchpathUninstallEntries).Count -eq 0)
    } while (-not $settled -and [DateTime]::UtcNow -lt $deadline)
    $userPathAfter = Get-UserPath
    $observation.uninstall = [ordered]@{
        exitCode = $uninstaller.ExitCode
        elapsedSeconds = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 1)
        installDirectoryRemains = (Test-Path -LiteralPath $installDirectory)
        uninstallEntriesRemain = (Get-FetchpathUninstallEntries).Count
        installDirectoryOnUserPath = (Test-PathListContains $userPathAfter $installDirectory)
        userPathAfter = $userPathAfter
        userPathRestored = ($userPathAfter -ceq $observation.preflight.userPathBefore)
        nativeHostKeysRemaining = @($nativeHostKeys | Where-Object { Test-Path -LiteralPath $_ }).Count
        downloadedFilesKept = (Test-Path -LiteralPath $cliDestination) -and (Test-Path -LiteralPath $appDestination) -and (Test-Path -LiteralPath $queuedFile)
        engineRunningBefore = $engineRunning
        fetchpathProcessesBefore = $processesBefore
        fetchpathProcessesAfter = Get-FetchpathProcesses
    }
    Assert-True (@($observation.uninstall.fetchpathProcessesAfter).Count -eq 0) 'A Fetchpath process outlived uninstall.'
    Assert-True ($observation.uninstall.exitCode -eq 0) "The uninstaller exited with code $($observation.uninstall.exitCode)."
    Assert-True (-not $observation.uninstall.installDirectoryRemains) 'Uninstall left the install directory behind.'
    Assert-True ($observation.uninstall.uninstallEntriesRemain -eq 0) 'Uninstall left an Apps & features entry.'
    Assert-True (-not $observation.uninstall.installDirectoryOnUserPath) 'Uninstall left the install directory on the user PATH.'
    Assert-True $observation.uninstall.userPathRestored 'The user PATH after uninstall is not byte-identical to the one before install.'
    Assert-True ($observation.uninstall.nativeHostKeysRemaining -eq 0) 'Uninstall left a browser host registration.'
    Assert-True $observation.uninstall.downloadedFilesKept 'Uninstall removed downloaded files.'
} catch {
    $observation.abortedWith = "$($_.Exception.Message) at $($_.InvocationInfo.PositionMessage)"
    # Which App Control policy refused a program, when one did.
    $observation.codeIntegrity = @(Get-WinEvent -LogName 'Microsoft-Windows-CodeIntegrity/Operational' -MaxEvents 6 -ErrorAction SilentlyContinue |
        ForEach-Object { "$($_.Id): $($_.Message -replace '\s+', ' ')" })
    $failures.Add("Run aborted: $($_.Exception.Message)")
} finally {
    if ($appProcess) { Stop-Process -Id $appProcess.Id -Force -ErrorAction SilentlyContinue }
    if ($fixture) { Stop-FixtureServer $fixture }
    $observation.failures = $failures
    $observation.passed = ($failures.Count -eq 0)
    $json = $observation | ConvertTo-Json -Depth 8
    [System.IO.File]::WriteAllText($OutputPath, $json, [System.Text.UTF8Encoding]::new($false))
    [System.IO.File]::WriteAllText($doneMarker, 'done')
}
