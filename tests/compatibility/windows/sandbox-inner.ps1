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
    [Parameter(Mandatory)] [string] $OutputPath,
    # FP-100: uninstall with /DELETEAPPDATA (the checkbox ticked) instead of keeping data.
    [switch] $DeleteData,
    # FP-099: the quiet component selection, as /COMPONENTS= takes it (for example
    # `cli`, or `desktop,cli,mcp,browser,torrent`). Empty runs the default, which
    # is Full on a fresh machine.
    [string] $Components = '',
    # FP-099: install this (earlier, Full) installer first, seed data, then run
    # the installer under test over it with -Components: the upgrade and
    # change-components path.
    [string] $BaselineInstallerPath = ''
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
$slowFixture = $null
$appProcess = $null
$installDirectory = Join-Path $env:LOCALAPPDATA 'Fetchpath'

# FP-099: what the selection under test must leave installed. Core is always there.
$want = [ordered]@{ desktop = $true; cli = $true; mcp = $true; browser = $true; torrent = $true }
$wantType = 'full'
if ($Components -and $Components.Trim().ToLowerInvariant() -ne 'full') {
    $wantType = 'custom'
    $names = @($Components.ToLowerInvariant().Split(',') | ForEach-Object { $_.Trim() })
    foreach ($key in @($want.Keys)) { $want[$key] = ($names -contains $key) }
    # The component rules the installer enforces interactively are refused in a
    # quiet install, so a valid selection here already has an interface.
}
$wantList = (@('core') + @($want.Keys | Where-Object { $want[$_] }) | Sort-Object) -join ','
$startMenuShortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Fetchpath.lnk'
$terminalShortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Fetchpath Terminal.lnk'
$desktopShortcut = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Fetchpath.lnk'
$uninstallKey = 'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Fetchpath'
$dataRoot = Join-Path $env:APPDATA 'app.fetchpath.desktop'
# What a terminal uses to reach the engine: the PATH entry exists while the
# terminal or AI agents is installed, else the full path.
$fp = if ($want.cli -or $want.mcp) { 'fetchpath' } else { "`"$installDirectory\fetchpath.exe`"" }
$hostName = 'com.fetchpath.browser'
$nativeHostKeys = @(
    "HKCU:\Software\Google\Chrome\NativeMessagingHosts\$hostName",
    "HKCU:\Software\Microsoft\Edge\NativeMessagingHosts\$hostName",
    "HKCU:\Software\Mozilla\NativeMessagingHosts\$hostName"
)

# A value stored with the uninstall entry, or $null.
function Get-Stored([string] $Name) {
    $properties = Get-ItemProperty -LiteralPath $uninstallKey -ErrorAction SilentlyContinue
    if (-not $properties) { return $null }
    $property = $properties.PSObject.Properties[$Name]
    if ($property) { return $property.Value } else { return $null }
}

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

    # Run a local copy, as a person runs the file they downloaded, rather than
    # one on the read-only mapped folder.
    $localInstaller = Join-Path $workDirectory ([System.IO.Path]::GetFileName($InstallerPath))
    Copy-Item -LiteralPath $InstallerPath -Destination $localInstaller -Force

    # ------------------------------------------- FP-099: refused selections ---
    # A selection the installer cannot use ends it with code 10 before a file is
    # touched. Run on the machine with nothing installed, only in a Custom run.
    if ($Components -and -not $BaselineInstallerPath) {
        Write-Progress-Line 'refused selections'
        $refused = [ordered]@{}
        foreach ($bad in @('mcp', 'browser,torrent', 'web', 'bogus', 'desktop,,cli', 'cli,', ',cli', 'full,cli', 'mcp,torrent')) {
            $attempt = Start-Process -FilePath $localInstaller -ArgumentList '/S', "/COMPONENTS=$bad" -PassThru -Wait
            $refused[$bad] = [ordered]@{
                exitCode = $attempt.ExitCode
                installDirectoryCreated = (Test-Path -LiteralPath $installDirectory)
                uninstallEntries = (Get-FetchpathUninstallEntries).Count
            }
            Assert-True ($attempt.ExitCode -eq 10) "/COMPONENTS=$bad exited with $($attempt.ExitCode), expected 10."
            Assert-True (-not $refused[$bad].installDirectoryCreated -and $refused[$bad].uninstallEntries -eq 0) "/COMPONENTS=$bad changed the machine."
        }
        $installLog = Join-Path $env:TEMP 'fetchpath-install.log'
        $observation.refusedSelections = $refused
        $observation.refusalLog = if (Test-Path -LiteralPath $installLog) { [System.IO.File]::ReadAllText($installLog) } else { $null }
        foreach ($reason in @('mcp needs desktop or cli', 'no usable interface', 'web is not part of this build', 'unknown component \[bogus\]', 'empty entry', 'unknown component \[full\]')) {
            Assert-True ($observation.refusalLog -match $reason) "The refusals did not log: $reason"
        }
    }

    # --------------------------------- FP-099: the earlier Full build, seeded ---
    if ($BaselineInstallerPath) {
        Write-Progress-Line 'baseline install'
        $baselineLocal = Join-Path $workDirectory 'baseline-setup.exe'
        Copy-Item -LiteralPath $BaselineInstallerPath -Destination $baselineLocal -Force
        $baseline = Start-Process -FilePath $baselineLocal -ArgumentList '/S' -PassThru -Wait
        $observation.baseline = [ordered]@{
            installerSha256 = (Get-FileHash -LiteralPath $baselineLocal -Algorithm SHA256).Hash.ToLowerInvariant()
            exitCode = $baseline.ExitCode
            desktopExecutable = (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath-desktop.exe'))
            browserHostExecutable = (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath-browser-host.exe'))
            torrentHelperExecutable = (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath-torrent-helper.exe'))
            nativeHostKeysRegistered = @($nativeHostKeys | Where-Object { Test-Path -LiteralPath $_ }).Count
            startMenuShortcut = (Test-Path -LiteralPath $startMenuShortcut)
            desktopShortcut = (Test-Path -LiteralPath $desktopShortcut)
            storedInstallType = Get-Stored 'FetchpathInstallType'
        }
        Assert-True ($observation.baseline.exitCode -eq 0) "The baseline installer exited with code $($observation.baseline.exitCode)."
        Assert-True ($observation.baseline.desktopExecutable -and $observation.baseline.browserHostExecutable -and $observation.baseline.torrentHelperExecutable) 'The baseline did not install every program.'
        Assert-True ($observation.baseline.nativeHostKeysRegistered -eq 3 -and $observation.baseline.startMenuShortcut -and $observation.baseline.desktopShortcut) 'The baseline did not register the keys and shortcuts.'
        Assert-True ($null -eq $observation.baseline.storedInstallType) 'The baseline already stores a selection; it must be the build from before components.'

        # Seed what an upgrade must keep: a finished download in history, a changed
        # setting, a file in the data folder, and a download that is still running.
        $fixture = Start-FixtureServer -Size (256 * 1024)
        $slowFixture = Start-FixtureServer -Size (3 * 1024 * 1024) -DelayMilliseconds 120
        $seedDirectory = Join-Path $workDirectory 'seed'
        $slowDirectory = Join-Path $workDirectory 'slow'
        foreach ($directory in @($seedDirectory, $slowDirectory)) { [System.IO.Directory]::CreateDirectory($directory) | Out-Null }
        $seedAdd = Invoke-FromNewTerminal 'seed-add' "fetchpath add $($fixture.BaseUrl)/seeded.bin --to `"$seedDirectory`" --wait" 120
        $seedSetting = Invoke-FromNewTerminal 'seed-setting' 'fetchpath settings auto-retry off'
        $settingBefore = Invoke-FromNewTerminal 'setting-before' 'fetchpath settings auto-retry'
        $historyBefore = Invoke-FromNewTerminal 'history-before' 'fetchpath history'
        [System.IO.Directory]::CreateDirectory($dataRoot) | Out-Null
        [System.IO.File]::WriteAllText((Join-Path $dataRoot 'fp099-sentinel.txt'), 'keep')
        $slowAdd = Invoke-FromNewTerminal 'slow-add' "fetchpath add $($slowFixture.BaseUrl)/slow.bin --to `"$slowDirectory`"" 60
        Start-Sleep -Seconds 3
        $observation.seed = [ordered]@{
            seedAdd = $seedAdd; seedSetting = $seedSetting; settingBefore = $settingBefore
            historyBefore = $historyBefore; slowAdd = $slowAdd
            seededFile = (Test-Path -LiteralPath (Join-Path $seedDirectory 'seeded.bin'))
            engineRunning = (@(Get-FetchpathProcesses | Where-Object { $_.image -eq 'fetchpath.exe' -and $_.commandLine -match '\sengine\s*$' }).Count -gt 0)
        }
        Assert-True ($seedAdd.exitCode -eq 0 -and $observation.seed.seededFile) "Seeding the finished download failed: $($seedAdd.output)"
        Assert-True ($settingBefore.output -match 'off') "The seeded setting did not read back off: $($settingBefore.output)"
        Assert-True ($historyBefore.output -match 'seeded\.bin') "History did not list the seeded download: $($historyBefore.output)"
        Assert-True $observation.seed.engineRunning 'No engine was running when the upgrade started.'
    }

    # ------------------------------------------------------------- install ---
    Write-Progress-Line 'install'
    $started = [DateTime]::UtcNow
    # The baseline's own WebView2 step left Microsoft's bootstrapper in TEMP; only the
    # installer under test may decide whether the file exists afterwards.
    $bootstrapper = Join-Path $env:TEMP 'MicrosoftEdgeWebview2Setup.exe'
    if (Test-Path -LiteralPath $bootstrapper) { Remove-Item -LiteralPath $bootstrapper -Force }
    $installArguments = @('/S')
    if ($Components) { $installArguments += "/COMPONENTS=$Components" }
    $installer = Start-Process -FilePath $localInstaller -ArgumentList $installArguments -PassThru -Wait
    $observation.install = [ordered]@{
        arguments = ($installArguments -join ' ')
        exitCode = $installer.ExitCode
        elapsedSeconds = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 1)
        uninstallEntries = Get-FetchpathUninstallEntries
        desktopExecutable = (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath-desktop.exe'))
        cliExecutable = (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath.exe'))
        browserHostExecutable = (Test-Path -LiteralPath (Join-Path $installDirectory "fetchpath-browser-host.exe"))
        browserExtensionFolder = (Test-Path -LiteralPath (Join-Path $installDirectory 'browser-extension'))
        torrentHelperExecutable = (Test-Path -LiteralPath (Join-Path $installDirectory "fetchpath-torrent-helper.exe"))
        installDirectoryOnUserPath = (Test-PathListContains (Get-UserPath) $installDirectory)
        nativeHostKeysRegistered = @($nativeHostKeys | Where-Object { Test-Path -LiteralPath $_ }).Count
        startMenuShortcut = (Test-Path -LiteralPath $startMenuShortcut)
        terminalShortcut = (Test-Path -LiteralPath $terminalShortcut)
        desktopShortcut = (Test-Path -LiteralPath $desktopShortcut)
        # The WebView2 step downloads Microsoft's bootstrapper only when the runtime is not registered.
        webview2BootstrapperDownloaded = (Test-Path -LiteralPath (Join-Path $env:TEMP 'MicrosoftEdgeWebview2Setup.exe'))
        storedInstallType = Get-Stored 'FetchpathInstallType'
        storedComponents = Get-Stored 'FetchpathComponents'
        storedSchema = Get-Stored 'FetchpathComponentsSchema'
        displayIcon = Get-Stored 'DisplayIcon'
        mainBinaryName = Get-Stored 'MainBinaryName'
        mediaToolsDownloaded = (Test-Path -LiteralPath (Join-Path $dataRoot 'media-tools'))
        setupLog = $(if (Test-Path -LiteralPath (Join-Path $env:TEMP 'fetchpath-install.log')) { [System.IO.File]::ReadAllText((Join-Path $env:TEMP 'fetchpath-install.log')) } else { $null })
    }
    $installed = $observation.install
    Assert-True ($installed.exitCode -eq 0) "The installer exited with code $($installed.exitCode)."
    Assert-True (@($installed.uninstallEntries).Count -eq 1) 'Expected exactly one per-user uninstall entry.'
    Assert-True ($installed.desktopExecutable -eq $want.desktop) "fetchpath-desktop.exe installed=$($installed.desktopExecutable), expected $($want.desktop)."
    Assert-True $installed.cliExecutable 'fetchpath.exe was not installed.'
    Assert-True ($installed.torrentHelperExecutable -eq $want.torrent) "fetchpath-torrent-helper.exe installed=$($installed.torrentHelperExecutable), expected $($want.torrent)."
    Assert-True ($installed.browserHostExecutable -eq $want.browser -and $installed.browserExtensionFolder -eq $want.browser) "Browser integration files installed=$($installed.browserHostExecutable), expected $($want.browser)."
    Assert-True ($installed.installDirectoryOnUserPath -eq ($want.cli -or $want.mcp)) "The user PATH entry is $($installed.installDirectoryOnUserPath), expected $($want.cli -or $want.mcp)."
    Assert-True ($installed.nativeHostKeysRegistered -eq $(if ($want.browser) { 3 } else { 0 })) `
        "Expected $(if ($want.browser) { 3 } else { 0 }) browser host registrations; found $($installed.nativeHostKeysRegistered)."
    Assert-True ($installed.startMenuShortcut -eq $want.desktop -and $installed.desktopShortcut -eq $want.desktop) 'The app shortcuts do not match the Desktop selection.'
    Assert-True ($installed.terminalShortcut -eq ($want.cli -and -not $want.desktop)) "The Fetchpath Terminal Start entry is $($installed.terminalShortcut), expected $($want.cli -and -not $want.desktop)."
    Assert-True ($installed.storedInstallType -eq $wantType -and $installed.storedComponents -eq $wantList -and $installed.storedSchema -eq 1) `
        "Stored selection is [$($installed.storedInstallType)] [$($installed.storedComponents)] [$($installed.storedSchema)], expected [$wantType] [$wantList] [1]."
    Assert-True ($installed.mainBinaryName -eq 'fetchpath.exe' -and $installed.displayIcon -match 'fetchpath\.exe') 'The uninstall entry is not keyed on fetchpath.exe.'
    Assert-True (-not $installed.mediaToolsDownloaded) 'A quiet install downloaded media tools.'
    if (-not $want.desktop) {
        Assert-True (-not $installed.webview2BootstrapperDownloaded) 'A selection without the desktop app ran the WebView2 step.'
    }

    # ---------------------------------------- FP-099: what an upgrade kept ---
    if ($BaselineInstallerPath) {
        Write-Progress-Line 'upgrade checks'
        $settingAfter = Invoke-FromNewTerminal 'setting-after' "$fp settings auto-retry"
        $historyAfter = Invoke-FromNewTerminal 'history-after' "$fp history"
        $slowFile = Join-Path $slowDirectory 'slow.bin'
        $slowDone = Wait-ForCondition {
            Invoke-FromNewTerminal 'slow-ls' "$fp ls" 30 | Out-Null
            if ((Test-Path -LiteralPath $slowFile -PathType Leaf) -and ((Get-Item -LiteralPath $slowFile).Length -eq 3MB)) { $true } else { $null }
        } 150 'the download that was running during the upgrade to finish'
        $observation.upgrade = [ordered]@{
            settingAfter = $settingAfter
            historyAfter = $historyAfter
            sentinelKept = (Test-Path -LiteralPath (Join-Path $dataRoot 'fp099-sentinel.txt'))
            dataFolderKept = (Test-Path -LiteralPath $dataRoot)
            seededFileKept = (Test-Path -LiteralPath (Join-Path $seedDirectory 'seeded.bin'))
            activeDownloadFinishedAfterUpgrade = [bool] $slowDone
            engineExecutableSha256 = (Get-FileHash -LiteralPath (Join-Path $installDirectory 'fetchpath.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        Assert-True ($settingAfter.output -match 'off') "The changed setting did not survive: $($settingAfter.output)"
        Assert-True ($historyAfter.output -match 'seeded\.bin') "History lost the seeded download: $($historyAfter.output)"
        Assert-True ($observation.upgrade.sentinelKept -and $observation.upgrade.seededFileKept) 'The upgrade removed data or a downloaded file.'
        Assert-True $observation.upgrade.activeDownloadFinishedAfterUpgrade 'The download that was running during the upgrade did not finish afterwards.'
    }

    # Empty input must reach the helper's own JSON error path. A missing MSVC
    # runtime instead prevents Windows from entering the helper at all.
    if ($want.torrent) {
        $helperSmoke = Invoke-FromNewTerminal 'torrent-helper' "`"$installDirectory\fetchpath-torrent-helper.exe`" <NUL" 30
        $observation.install.torrentHelperSmoke = $helperSmoke
        Assert-True ($helperSmoke.exitCode -eq 1 -and $helperSmoke.output -match 'request.invalid') `
            "The torrent helper did not start cleanly: $($helperSmoke.output)"
    }

    # ------------------------------------------------ CLI in a new terminal ---
    Write-Progress-Line "installed with exit code $($observation.install.exitCode); CLI"
    if (-not $fixture) { $fixture = Start-FixtureServer -Size (256 * 1024) }
    $observation.cli = [ordered]@{}
    $help = Invoke-FromNewTerminal 'help' "$fp --help"
    $observation.cli.help = $help
    Assert-True ($help.exitCode -eq 0) "``fetchpath --help`` from a new terminal exited with $($help.exitCode)."
    Assert-True ($help.output -and $help.output -match 'download') '`fetchpath --help` printed no usage text.'
    $version = Invoke-FromNewTerminal 'version' "$fp --version"
    $observation.cli.version = $version
    Assert-True ($version.exitCode -eq 0 -and $version.output -match '0\.1\.0') "``fetchpath --version`` did not report 0.1.0: $($version.output)"

    $cliDestination = Join-Path $workDirectory 'cli-download.bin'
    $cliDownload = Invoke-FromNewTerminal 'download' "$fp download $($fixture.BaseUrl)/cli.bin `"$cliDestination`" --json" 120
    $observation.cli.download = $cliDownload
    $observation.cli.downloadedBytes = if (Test-Path -LiteralPath $cliDestination) { (Get-Item -LiteralPath $cliDestination).Length } else { $null }
    Assert-True ($cliDownload.exitCode -eq 0) "The CLI download exited with $($cliDownload.exitCode): $($cliDownload.output)"
    Assert-True ($observation.cli.downloadedBytes -eq 262144) "The CLI saved $($observation.cli.downloadedBytes) bytes, expected 262144."

    # The queue belongs to the engine, which the command starts on demand.
    $queueDirectory = Join-Path $workDirectory 'queued'
    [System.IO.Directory]::CreateDirectory($queueDirectory) | Out-Null
    $queued = Invoke-FromNewTerminal 'queue' "$fp add $($fixture.BaseUrl)/queued.bin --to `"$queueDirectory`" --wait" 120
    $observation.cli.queuedDownload = $queued
    $queuedFile = Join-Path $queueDirectory 'queued.bin'
    $observation.cli.queuedBytes = if (Test-Path -LiteralPath $queuedFile) { (Get-Item -LiteralPath $queuedFile).Length } else { $null }
    Assert-True ($queued.exitCode -eq 0) "``fetchpath add --wait`` exited with $($queued.exitCode): $($queued.output)"
    Assert-True ($observation.cli.queuedBytes -eq 262144) "The engine saved $($observation.cli.queuedBytes) bytes, expected 262144."
    $status = Invoke-FromNewTerminal 'engine-status' "$fp engine status"
    $observation.cli.engineStatus = $status
    Assert-True ($status.exitCode -eq 0) "``fetchpath engine status`` exited with $($status.exitCode): $($status.output)"

    # ----------------------------------------------------------- /UPDATE ---
    # FP-099: the desktop's in-app updater runs setup with /UPDATE. It keeps the
    # stored selection, ignores /COMPONENTS (given here to the opposite of what is
    # installed) and removes nothing; setup lifts the engine hold when it ends.
    Write-Progress-Line 'update'
    function Get-InstalledState {
        return [ordered]@{
            type = Get-Stored 'FetchpathInstallType'
            list = Get-Stored 'FetchpathComponents'
            desktop = (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath-desktop.exe'))
            engine = (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath.exe'))
            browser = (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath-browser-host.exe'))
            torrent = (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath-torrent-helper.exe'))
            nativeHostKeys = @($nativeHostKeys | Where-Object { Test-Path -LiteralPath $_ }).Count
            onPath = (Test-PathListContains (Get-UserPath) $installDirectory)
            shortcuts = @($startMenuShortcut, $terminalShortcut, $desktopShortcut | Where-Object { Test-Path -LiteralPath $_ }).Count
        }
    }
    $beforeUpdate = Get-InstalledState
    $updateSwitch = if ($want.desktop) { '/COMPONENTS=cli' } else { '/COMPONENTS=full' }
    $update = Start-Process -FilePath $localInstaller -ArgumentList '/S', '/UPDATE', $updateSwitch -PassThru -Wait
    $afterUpdate = Get-InstalledState
    $afterUpdateEngine = Invoke-FromNewTerminal 'after-update' "$fp ls" 60
    $observation.update = [ordered]@{
        arguments = "/S /UPDATE $updateSwitch"
        exitCode = $update.ExitCode
        before = $beforeUpdate
        after = $afterUpdate
        engineStartsAfterUpdate = $afterUpdateEngine
    }
    Assert-True ($update.ExitCode -eq 0) "The /UPDATE setup exited with $($update.ExitCode)."
    Assert-True ((ConvertTo-Json $beforeUpdate) -ceq (ConvertTo-Json $afterUpdate)) '/UPDATE changed the installed components, files, shortcuts or PATH.'
    Assert-True ($afterUpdateEngine.exitCode -eq 0) "The engine did not start after /UPDATE (the update hold may not have been lifted): $($afterUpdateEngine.output)"

    $appDestination = Join-Path $workDirectory 'app-download.bin'
    if ($want.desktop) {
        # --------------------------------------------------- desktop download ---
        Write-Progress-Line 'desktop download'
        $appProcess = Start-Process -FilePath (Join-Path $installDirectory 'fetchpath-desktop.exe') -PassThru
        $renderer = Get-RendererRoot $appProcess 90
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

    }

    # ----------------------------------------------------------- uninstall ---
    Write-Progress-Line 'uninstall'
    # Closing the window leaves the engine running for its idle grace, so
    # uninstall meets a live engine, as a person uninstalling right away would.
    if (-not $want.desktop) {
        # No app window to close: a command a moment ago leaves the engine running for its idle grace.
        Invoke-FromNewTerminal 'wake-engine' "$fp ls" 30 | Out-Null
    }
    $processesBefore = Get-FetchpathProcesses
    $engineRunning = @($processesBefore | Where-Object { $_.image -eq 'fetchpath.exe' -and $_.commandLine -match '\sengine\s*$' }).Count -gt 0
    Assert-True $engineRunning 'No engine was running when uninstall started.'
    $started = [DateTime]::UtcNow
    $dataRoots = @((Join-Path $env:APPDATA 'app.fetchpath.desktop'), (Join-Path $env:LOCALAPPDATA 'app.fetchpath.desktop'))
    $outsideSentinel = Join-Path $workDirectory 'outside-owned-roots\sentinel.txt'
    if ($DeleteData) {
        # A junction inside an owned root that points at a folder outside both
        # roots: removal must delete the link and leave the folder alone.
        [System.IO.Directory]::CreateDirectory((Split-Path $outsideSentinel)) | Out-Null
        [System.IO.File]::WriteAllText($outsideSentinel, 'keep')
        $junction = Join-Path $dataRoots[0] 'junction-to-outside'
        cmd.exe /c mklink /J "$junction" (Split-Path $outsideSentinel) | Out-Null
        Assert-True (Test-Path -LiteralPath $junction) 'Could not create the sentinel junction.'
    }
    $uninstallArguments = if ($DeleteData) { '/S /DELETEAPPDATA' } else { '/S' }
    $uninstaller = Start-Process -FilePath (Join-Path $installDirectory 'uninstall.exe') -ArgumentList $uninstallArguments -PassThru
    $uninstaller.WaitForExit(120000) | Out-Null
    $deadline = [DateTime]::UtcNow.AddSeconds(90)
    do {
        Start-Sleep -Seconds 2
        $settled = (-not (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath.exe'))) -and
            ((Get-FetchpathUninstallEntries).Count -eq 0)
    } while (-not $settled -and [DateTime]::UtcNow -lt $deadline)
    # FP-100: the uninstaller's relocated copy may still be finishing after the
    # first process exits. Its last log line says it is done and which branch ran.
    $dataLog = Join-Path $env:TEMP 'fetchpath-uninstall.log'
    $logDeadline = [DateTime]::UtcNow.AddSeconds(60)
    do {
        Start-Sleep -Seconds 1
        $dataLogText = if (Test-Path -LiteralPath $dataLog) { [System.IO.File]::ReadAllText($dataLog) } else { '' }
    } while (-not ($dataLogText -like '*remove: wipe=*' -and ($dataLogText -notlike '*remove: wipe=1*' -or $dataLogText -like '*script exit=*')) -and [DateTime]::UtcNow -lt $logDeadline)
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
        deleteData = [bool]$DeleteData
        dataLog = $dataLogText
        dataRootsRemaining = @($dataRoots | Where-Object { Test-Path -LiteralPath $_ }).Count
        outsideSentinelKept = (-not $DeleteData) -or ((Test-Path -LiteralPath $outsideSentinel) -and ([System.IO.File]::ReadAllText($outsideSentinel) -eq 'keep'))
        installPathKeyRemaining = (Test-Path -LiteralPath 'HKCU:\SOFTWARE\Fetchpath contributors')
        downloadedFilesKept = (Test-Path -LiteralPath $cliDestination) -and ((-not $want.desktop) -or (Test-Path -LiteralPath $appDestination)) -and (Test-Path -LiteralPath $queuedFile)
        shortcutsRemaining = @($startMenuShortcut, $terminalShortcut, $desktopShortcut | Where-Object { Test-Path -LiteralPath $_ }).Count
        storedSelectionRemains = ($null -ne (Get-Stored 'FetchpathComponents'))
        roamingDataKept = (Test-Path -LiteralPath $dataRoots[0])
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
    Assert-True ($observation.uninstall.shortcutsRemaining -eq 0) 'Uninstall left a Start menu or desktop shortcut.'
    Assert-True (-not $observation.uninstall.storedSelectionRemains) 'Uninstall left the stored component selection.'
    if ($DeleteData) {
        Assert-True ($observation.uninstall.dataRootsRemaining -eq 0) 'Delete-data uninstall left an application data folder.'
        Assert-True $observation.uninstall.outsideSentinelKept 'Delete-data uninstall removed a file outside the owned folders, through a junction.'
    } else {
        # The local folder (WebView2 profile, pairing) exists only once the app window has run.
        $expectedRoots = if ($want.desktop -and -not $BaselineInstallerPath) { 2 } else { 1 }
        Assert-True ($observation.uninstall.dataRootsRemaining -ge $expectedRoots -and $observation.uninstall.roamingDataKept) 'A data-keeping uninstall removed an application data folder.'
    }
} catch {
    $observation.abortedWith = "$($_.Exception.Message) at $($_.InvocationInfo.PositionMessage)"
    # Which App Control policy refused a program, when one did.
    $observation.codeIntegrity = @(Get-WinEvent -LogName 'Microsoft-Windows-CodeIntegrity/Operational' -MaxEvents 6 -ErrorAction SilentlyContinue |
        ForEach-Object { "$($_.Id): $($_.Message -replace '\s+', ' ')" })
    $failures.Add("Run aborted: $($_.Exception.Message)")
} finally {
    if ($appProcess) { Stop-Process -Id $appProcess.Id -Force -ErrorAction SilentlyContinue }
    if ($fixture) { Stop-FixtureServer $fixture }
    if ($slowFixture) { Stop-FixtureServer $slowFixture }
    $observation.components = [ordered]@{ requested = $Components; expectedType = $wantType; expectedList = $wantList; baseline = [bool]$BaselineInstallerPath }
    $observation.failures = $failures
    $observation.passed = ($failures.Count -eq 0)
    $json = $observation | ConvertTo-Json -Depth 8
    [System.IO.File]::WriteAllText($OutputPath, $json, [System.Text.UTF8Encoding]::new($false))
    [System.IO.File]::WriteAllText($doneMarker, 'done')
}
