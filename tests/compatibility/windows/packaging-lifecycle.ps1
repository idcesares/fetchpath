# Fetchpath Windows install / upgrade / uninstall lifecycle (FP-017, A12).
#
# This performs a REAL per-user installation on the machine it runs on. It
# writes to %LOCALAPPDATA%, %APPDATA% and HKCU. It is written to leave the
# machine as it found it:
#
#   * it refuses to run if Fetchpath is already installed, rather than
#     uninstalling somebody's copy;
#   * it backs up a pre-existing %APPDATA%\app.fetchpath.desktop before touching
#     it and restores it at the end;
#   * if the application data directory did not exist beforehand, it removes the
#     one this run created.
#
# Sequence: install -> seed a real queue -> close to tray -> second-launch
# behaviour -> upgrade over the running install -> verify the retained queue ->
# uninstall -> record residue.
#
# Emits JSON to -OutputPath and throws on any failure.

[CmdletBinding()]
param(
    [string] $InstallerPath,
    [string] $UpgradeInstallerPath,
    [string] $OutputPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'uia-common.ps1')

$repositoryRoot = Get-RepositoryRoot $PSScriptRoot
if (-not $InstallerPath) {
    $InstallerPath = Join-Path $repositoryRoot 'target\release\bundle\nsis\Fetchpath_0.1.0_x64-setup.exe'
}
if (-not $UpgradeInstallerPath) {
    $UpgradeInstallerPath = Join-Path $repositoryRoot 'target\release\bundle\nsis\Fetchpath_0.1.1_x64-setup.exe'
}
if (-not $OutputPath) {
    $OutputPath = Join-Path $repositoryRoot 'docs\development\evidence\windows\packaging-lifecycle.json'
}
foreach ($path in @($InstallerPath, $UpgradeInstallerPath)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Installer not found: $path" }
}

$appDataDirectory = Join-Path $env:APPDATA 'app.fetchpath.desktop'
$queuePath = Join-Path $appDataDirectory 'queue-v1.json'
$expectedInstallDirectory = Join-Path $env:LOCALAPPDATA 'Fetchpath'
$startMenuShortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Fetchpath.lnk'
$desktopShortcut = Join-Path ([System.Environment]::GetFolderPath('Desktop')) 'Fetchpath.lnk'
$uninstallRoots = @(
    'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall',
    'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall',
    'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall'
)
$secretQueryValue = 'fp017-private-query-value'
# Tauri's NSIS template records the install directory here and removes it only
# when the uninstaller's "Delete the application data" checkbox is ticked.
$manufacturerKey = 'HKCU:\SOFTWARE\Fetchpath contributors'
$installPathKey = "$manufacturerKey\Fetchpath"
$localAppDataBundleDirectory = Join-Path $env:LOCALAPPDATA 'app.fetchpath.desktop'

$workDirectory = Join-Path $repositoryRoot 'work\fp017-packaging'
if (-not $workDirectory.StartsWith($repositoryRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Refusing to use a work directory outside the repository.'
}
if (Test-Path -LiteralPath $workDirectory) { Remove-Item -LiteralPath $workDirectory -Recurse -Force }
[System.IO.Directory]::CreateDirectory($workDirectory) | Out-Null
$backupDirectory = Join-Path $workDirectory 'preexisting-appdata'

$failures = [System.Collections.Generic.List[string]]::new()
function Assert-True([bool] $Condition, [string] $Message) {
    if (-not $Condition) { $failures.Add($Message) }
}

function Get-RegistryValue($Properties, [string] $Name) {
    $property = $Properties.PSObject.Properties[$Name]
    if (-not $property) { return $null }
    $value = $property.Value
    # NSIS writes InstallLocation and UninstallString wrapped in double quotes.
    if ($value -is [string]) { return $value.Trim('"') }
    return $value
}

function Get-FetchpathUninstallEntries {
    $entries = [System.Collections.Generic.List[object]]::new()
    foreach ($root in $uninstallRoots) {
        if (-not (Test-Path -LiteralPath $root)) { continue }
        foreach ($key in Get-ChildItem -LiteralPath $root -ErrorAction SilentlyContinue) {
            $properties = Get-ItemProperty -LiteralPath $key.PSPath -ErrorAction SilentlyContinue
            if (-not $properties) { continue }
            $displayName = Get-RegistryValue $properties 'DisplayName'
            if (-not $displayName -or $displayName -notlike '*Fetchpath*') { continue }
            $entries.Add([pscustomobject]@{
                registryKey = ($key.PSPath -replace '^Microsoft\.PowerShell\.Core\\Registry::', '')
                displayName = $displayName
                displayVersion = Get-RegistryValue $properties 'DisplayVersion'
                publisher = Get-RegistryValue $properties 'Publisher'
                installLocation = Get-RegistryValue $properties 'InstallLocation'
                uninstallString = Get-RegistryValue $properties 'UninstallString'
                estimatedSizeKb = Get-RegistryValue $properties 'EstimatedSize'
            })
        }
    }
    return , $entries.ToArray()
}

# Each queue card is an <article role="listitem"> whose aria-label is
# "<filename>, <status>", so the whole queue can be read as accessible names.
function Get-JobCardNames($Renderer) {
    $list = Find-ById $Renderer 'job-list'
    if (-not $list) { return , @() }
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::ListItem
    )
    $items = $list.FindAll([System.Windows.Automation.TreeScope]::Subtree, $condition)
    $names = for ($index = 0; $index -lt $items.Count; $index++) {
        $name = $items.Item($index).Current.Name
        if ($name) { $name }
    }
    return , @($names)
}

function Add-QueuedDownload($Renderer, $Process, [string] $Url, [string] $Destination) {
    Set-Field $Renderer 'url' $Url
    Set-Field $Renderer 'destination' $Destination
    Start-Sleep -Milliseconds 250
    Invoke-Control $Renderer 'start-download' $Process
    Start-Sleep -Milliseconds 400
}

function Invoke-SilentInstaller([string] $Path, [string] $Label) {
    $started = [DateTime]::UtcNow
    $process = Start-Process -FilePath $Path -ArgumentList '/S' -PassThru -Wait
    # An ordered dictionary, not a pscustomobject, so observations can be added.
    return [ordered]@{
        label = $Label
        installer = $Path
        exitCode = $process.ExitCode
        elapsedSeconds = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 1)
    }
}

function Get-InstalledTree([string] $Directory) {
    if (-not $Directory -or -not (Test-Path -LiteralPath $Directory)) { return , @() }
    $files = Get-ChildItem -LiteralPath $Directory -Recurse -File -ErrorAction SilentlyContinue |
        ForEach-Object { $_.FullName.Substring($Directory.Length).TrimStart('\') } |
        Sort-Object
    return , @($files)
}

$observation = [ordered]@{}
$observation.osBuild = [string] [System.Environment]::OSVersion.Version
$observation.machineArchitecture = $env:PROCESSOR_ARCHITECTURE
$observation.installerUnderTest = $InstallerPath
$observation.upgradeInstaller = $UpgradeInstallerPath
$observation.installerSha256 = (Get-FileHash -LiteralPath $InstallerPath -Algorithm SHA256).Hash.ToLowerInvariant()
$observation.upgradeInstallerSha256 = (Get-FileHash -LiteralPath $UpgradeInstallerPath -Algorithm SHA256).Hash.ToLowerInvariant()

# --------------------------------------------------------------- preflight ---
$preexistingEntries = Get-FetchpathUninstallEntries
$appDataExistedBefore = Test-Path -LiteralPath $appDataDirectory
$observation.preflight = [ordered]@{
    existingUninstallEntries = $preexistingEntries
    appDataExistedBefore = $appDataExistedBefore
    installDirectoryExistedBefore = (Test-Path -LiteralPath $expectedInstallDirectory)
    manufacturerRegistryKeyExistedBefore = (Test-Path -LiteralPath $manufacturerKey)
    localAppDataBundleDirectoryExistedBefore = (Test-Path -LiteralPath $localAppDataBundleDirectory)
    webview2RuntimeVersion = $null
}
foreach ($key in @(
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
        'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
        'HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}')) {
    if (Test-Path -LiteralPath $key) {
        $observation.preflight.webview2RuntimeVersion = (Get-ItemProperty -LiteralPath $key).pv
        break
    }
}

if ($preexistingEntries.Count -gt 0 -or $observation.preflight.installDirectoryExistedBefore) {
    throw 'Fetchpath is already installed on this machine. This script will not remove an existing installation; uninstall it manually first.'
}
if ($observation.preflight.manufacturerRegistryKeyExistedBefore) {
    throw "A previous Fetchpath install left $manufacturerKey behind. Remove it before running this script, so its residue is not attributed to this run."
}

$restoreAppData = $false
if ($appDataExistedBefore) {
    [System.IO.Directory]::CreateDirectory($backupDirectory) | Out-Null
    Copy-Item -LiteralPath $appDataDirectory -Destination $backupDirectory -Recurse -Force
    $restoreAppData = $true
    $observation.preflight.appDataBackup = $backupDirectory
    Remove-Item -LiteralPath $appDataDirectory -Recurse -Force
}

$fixture = $null
$seedProcess = $null
$verifyProcess = $null
$installed = $false

try {
    # ----------------------------------------------------------- 1. install ---
    $observation.install = Invoke-SilentInstaller $InstallerPath 'install 0.1.0'
    $installed = $true
    Assert-True ($observation.install.exitCode -eq 0) "The installer exited with code $($observation.install.exitCode)."

    $entries = Get-FetchpathUninstallEntries
    $observation.install.uninstallEntries = $entries
    Assert-True ($entries.Count -eq 1) "Expected exactly one uninstall entry, found $($entries.Count)."
    Assert-True (@($entries | Where-Object { $_.registryKey -like 'HKEY_CURRENT_USER*' }).Count -eq 1) `
        'The uninstall entry is not under HKEY_CURRENT_USER, so this was not a per-user install.'

    $installDirectory = if ($entries.Count -ge 1 -and $entries[0].installLocation) { $entries[0].installLocation.TrimEnd('\') } else { $expectedInstallDirectory }
    $observation.install.installDirectory = $installDirectory
    $observation.install.installDirectoryIsPerUser = $installDirectory.StartsWith($env:LOCALAPPDATA, [System.StringComparison]::OrdinalIgnoreCase)
    Assert-True $observation.install.installDirectoryIsPerUser `
        "The install directory '$installDirectory' is not under %LOCALAPPDATA%."

    $observation.install.files = Get-InstalledTree $installDirectory
    # Tauri names the main binary after the Cargo bin target, not productName,
    # so the installed executable is fetchpath-desktop.exe. Only the shortcut
    # and the Apps & features entry are called "Fetchpath".
    $installedExecutable = Join-Path $installDirectory 'fetchpath-desktop.exe'
    Assert-True (Test-Path -LiteralPath $installedExecutable -PathType Leaf) `
        "The application executable was not installed at $installedExecutable."
    $observation.install.browserHostInstalled = (Test-Path -LiteralPath (Join-Path $installDirectory 'fetchpath-browser-host.exe'))
    $observation.install.thirdPartyNoticesInstalled = @($observation.install.files | Where-Object { $_ -like '*THIRD-PARTY-NOTICES.md' }).Count -gt 0
    Assert-True $observation.install.thirdPartyNoticesInstalled 'THIRD-PARTY-NOTICES.md was not delivered beside the executable.'
    $observation.install.uninstallerPresent = (Test-Path -LiteralPath (Join-Path $installDirectory 'uninstall.exe'))
    Assert-True $observation.install.uninstallerPresent 'No uninstall.exe was installed.'
    $observation.install.startMenuShortcut = (Test-Path -LiteralPath $startMenuShortcut)
    $observation.install.desktopShortcut = (Test-Path -LiteralPath $desktopShortcut)
    Assert-True ($observation.install.startMenuShortcut -or $observation.install.desktopShortcut) `
        'The installer created neither a Start menu nor a desktop shortcut.'
    # A shortcut that points at nothing is worse than no shortcut, so the target
    # of each one the installer created is resolved and checked.
    $shell = New-Object -ComObject WScript.Shell
    $shortcutTargets = [ordered]@{}
    foreach ($pair in @(@{ label = 'startMenu'; path = $startMenuShortcut }, @{ label = 'desktop'; path = $desktopShortcut })) {
        if (-not (Test-Path -LiteralPath $pair.path)) { continue }
        $target = $shell.CreateShortcut($pair.path).TargetPath
        $shortcutTargets[$pair.label] = $target
        Assert-True ($target -eq $installedExecutable) `
            "The $($pair.label) shortcut points at '$target' instead of '$installedExecutable'."
    }
    $observation.install.shortcutTargets = $shortcutTargets
    $observation.install.executableIsSigned = ((Get-AuthenticodeSignature -LiteralPath $installedExecutable).Status.ToString())
    $observation.install.installerIsSigned = ((Get-AuthenticodeSignature -LiteralPath $InstallerPath).Status.ToString())
    $observation.install.installPathRegistryValue = if (Test-Path -LiteralPath $installPathKey) {
        (Get-ItemProperty -LiteralPath $installPathKey).'(default)'
    } else { $null }
    Assert-True ($observation.install.installPathRegistryValue -eq $installDirectory) `
        "The recorded install path is '$($observation.install.installPathRegistryValue)', expected '$installDirectory'."

    # -------------------------------------------------------- 2. seed queue ---
    $fixture = Start-FixtureServer -Size (256 * 1024)
    $closedPort = Get-ClosedPort
    $completedDestination = Join-Path $workDirectory 'retained-complete.bin'
    $failedDestination = Join-Path $workDirectory 'retained-failed.bin'
    $privateDestination = Join-Path $workDirectory 'retained-private.bin'

    $seedProcess = Start-Process -FilePath $installedExecutable -PassThru
    $renderer = Get-RendererRoot $seedProcess
    $observation.seed = [ordered]@{}

    # Native compatibility facts about the installed process, recorded while it
    # is running: DPI awareness, the window class, and the effective window DPI.
    $observation.nativeCompatibility = [ordered]@{
        windowClass = [FetchpathUia]::ClassName($seedProcess.MainWindowHandle)
        processDpiAwareness = [FetchpathUia]::DpiAwareness($seedProcess.Handle)
        windowDpi = [int] [FetchpathUia]::WindowDpi($seedProcess.MainWindowHandle)
        webview2RuntimeVersion = $observation.preflight.webview2RuntimeVersion
    }
    Assert-True ($observation.nativeCompatibility.processDpiAwareness -eq 2) `
        "The installed process reports DPI awareness $($observation.nativeCompatibility.processDpiAwareness); per-monitor awareness is 2."
    Assert-True ($observation.nativeCompatibility.windowDpi -ge 96) `
        "The window reported a DPI of $($observation.nativeCompatibility.windowDpi)."

    Add-QueuedDownload $renderer $seedProcess "$($fixture.BaseUrl)/retained.bin" $completedDestination
    $observation.seed.completedStatus = Wait-ForStatus $renderer @('Complete', 'Needs attention') 60
    Assert-True ($observation.seed.completedStatus -eq 'Complete') `
        "The seeded download ended in '$($observation.seed.completedStatus)'."

    Add-QueuedDownload $renderer $seedProcess "http://127.0.0.1:$closedPort/safe-source.bin" $failedDestination
    Add-QueuedDownload $renderer $seedProcess "http://127.0.0.1:$closedPort/private.bin?token=$secretQueryValue" $privateDestination

    $observation.seed.cardsBeforeUpgrade = Wait-ForCondition {
        $names = Get-JobCardNames $renderer
        if (@($names).Count -eq 3 -and (@($names | Where-Object { $_ -match 'Needs attention' }).Count -eq 2)) { $names } else { $null }
    } 60 'three seeded queue items with two in a recovery state'

    Assert-True (Test-Path -LiteralPath $queuePath) "No persisted queue was written to $queuePath."
    $queueBeforeUpgrade = Get-Content -LiteralPath $queuePath -Raw
    $observation.seed.queueBytesBeforeUpgrade = $queueBeforeUpgrade.Length
    $observation.seed.queueRecordCountBeforeUpgrade = ($queueBeforeUpgrade | ConvertFrom-Json).records.Count
    $observation.seed.privateQueryValueInQueueFile = $queueBeforeUpgrade.Contains($secretQueryValue)
    Assert-True ($observation.seed.queueRecordCountBeforeUpgrade -eq 3) `
        "The persisted queue holds $($observation.seed.queueRecordCountBeforeUpgrade) records, expected 3."
    Assert-True (-not $observation.seed.privateQueryValueInQueueFile) `
        'The private query value was written into the persisted queue.'

    # ------------------------------------------- 3. close to tray, relaunch ---
    # WM_CLOSE is the same request the title bar's close button sends. It is
    # posted rather than typed as Alt+F4, because synthetic global input only
    # reaches the foreground window and would be discarded here.
    $mainWindow = $seedProcess.MainWindowHandle
    [FetchpathUia]::PostClose($mainWindow)
    Start-Sleep -Seconds 3
    $seedProcess.Refresh()
    $observation.trayBehaviour = [ordered]@{
        processAliveAfterWindowClose = (-not $seedProcess.HasExited)
        windowVisibleAfterClose = [FetchpathUia]::IsWindowVisible($mainWindow)
    }
    Assert-True $observation.trayBehaviour.processAliveAfterWindowClose `
        'Closing the window terminated the process instead of keeping the queue in the notification area.'
    Assert-True (-not $observation.trayBehaviour.windowVisibleAfterClose) `
        'Closing the window left it visible.'

    $secondLaunch = Start-Process -FilePath $installedExecutable -PassThru
    $secondLaunch.WaitForExit(20000) | Out-Null
    $secondLaunch.Refresh()
    $seedProcess.Refresh()
    $observation.singleInstance = [ordered]@{
        secondLaunchExited = $secondLaunch.HasExited
        secondLaunchExitCode = if ($secondLaunch.HasExited) { $secondLaunch.ExitCode } else { $null }
        firstInstanceStillRunning = (-not $seedProcess.HasExited)
        firstWindowRestored = [FetchpathUia]::IsWindowVisible($mainWindow)
        fetchpathProcessCount = @(Get-Process -Name 'Fetchpath' -ErrorAction SilentlyContinue).Count
    }
    if (-not $secondLaunch.HasExited) { Stop-Process -Id $secondLaunch.Id -Force }
    Assert-True $observation.singleInstance.secondLaunchExited `
        'A second launch did not exit; two instances would both own the persisted queue.'
    Assert-True $observation.singleInstance.firstInstanceStillRunning `
        'The second launch terminated the running instance.'
    Assert-True $observation.singleInstance.firstWindowRestored `
        'The second launch did not bring the running instance back from the notification area.'

    # ----------------------------- 4. upgrade over the running installation ---
    $observation.upgrade = Invoke-SilentInstaller $UpgradeInstallerPath 'upgrade to 0.1.1 with the app running'
    Start-Sleep -Seconds 2
    $seedProcess.Refresh()
    $observation.upgrade.runningInstanceStoppedByInstaller = $seedProcess.HasExited
    if (-not $seedProcess.HasExited) {
        Stop-Process -Id $seedProcess.Id -Force
        Start-Sleep -Seconds 1
    }
    $seedProcess = $null
    Assert-True ($observation.upgrade.exitCode -eq 0) "The upgrade exited with code $($observation.upgrade.exitCode)."
    $upgradeEntries = Get-FetchpathUninstallEntries
    $observation.upgrade.uninstallEntries = $upgradeEntries
    Assert-True ($upgradeEntries.Count -eq 1) `
        "After the upgrade there are $($upgradeEntries.Count) uninstall entries; an upgrade must replace, not accumulate."
    Assert-True ($upgradeEntries.Count -ge 1 -and $upgradeEntries[0].displayVersion -eq '0.1.1') `
        "The recorded version after the upgrade is '$(if ($upgradeEntries.Count -ge 1) { $upgradeEntries[0].displayVersion })', expected 0.1.1."
    $observation.upgrade.files = Get-InstalledTree $installDirectory

    # ------------------------------------------- 5. the queue must be intact ---
    Assert-True (Test-Path -LiteralPath $queuePath) 'The upgrade removed the persisted queue.'
    $queueAfterUpgrade = Get-Content -LiteralPath $queuePath -Raw
    $observation.retainedQueue = [ordered]@{
        queueFileUnchangedByUpgrade = ($queueAfterUpgrade -eq $queueBeforeUpgrade)
        privateQueryValueInQueueFile = $queueAfterUpgrade.Contains($secretQueryValue)
    }
    Assert-True $observation.retainedQueue.queueFileUnchangedByUpgrade `
        'The upgrade modified the persisted queue file.'

    $verifyProcess = Start-Process -FilePath $installedExecutable -PassThru
    $renderer = Get-RendererRoot $verifyProcess
    $recoveredCards = Wait-ForCondition {
        $names = Get-JobCardNames $renderer
        if (@($names).Count -eq 3) { $names } else { $null }
    } 60 'the three retained queue items to be restored after the upgrade'
    $observation.retainedQueue.cardsAfterUpgrade = $recoveredCards
    Assert-True (@($recoveredCards | Where-Object { $_ -match 'retained-complete\.bin, Complete' }).Count -eq 1) `
        "The completed history item did not survive the upgrade. Cards: $($recoveredCards -join ' | ')"
    Assert-True (@($recoveredCards | Where-Object { $_ -match 'retained-failed\.bin, Needs attention' }).Count -eq 1) `
        "The safe-source recovery item did not survive the upgrade. Cards: $($recoveredCards -join ' | ')"
    Assert-True (@($recoveredCards | Where-Object { $_ -match 'retained-private\.bin, Link needed' }).Count -eq 1) `
        "The private-source item must require an explicit refresh after restore. Cards: $($recoveredCards -join ' | ')"

    $observation.retainedQueue.privateSourceVisibleText = Get-ControlText $renderer 'queue-search'
    $privateCardTexts = [System.Collections.Generic.List[string]]::new()
    $all = Get-DescendantElements $renderer
    for ($index = 0; $index -lt $all.Count; $index++) {
        $name = $all.Item($index).Current.Name
        if ($name -and $name.Contains($secretQueryValue)) { $privateCardTexts.Add($name) }
    }
    $observation.retainedQueue.privateQueryValueVisibleInUi = ($privateCardTexts.Count -gt 0)
    Assert-True (-not $observation.retainedQueue.privateQueryValueVisibleInUi) `
        'The private query value is exposed in the restored user interface.'

    Stop-Process -Id $verifyProcess.Id -Force
    Start-Sleep -Seconds 1
    $verifyProcess = $null

    # --------------------------------------------------------- 6. uninstall ---
    # Run the uninstaller the way Apps & features does: no NSIS `_?=`, so it
    # relocates itself to the temp directory and can remove its own directory.
    # That makes the call asynchronous, so completion is polled rather than
    # waited on, and the residue below is what a real uninstall leaves.
    $uninstaller = Join-Path $installDirectory 'uninstall.exe'
    $tauriConfig = Get-Content -LiteralPath (Join-Path $repositoryRoot 'apps\desktop\src-tauri\tauri.conf.json') -Raw | ConvertFrom-Json
    $dataRemovalHook = $tauriConfig.bundle.windows.nsis.installerHooks
    Assert-True ([bool] $dataRemovalHook) `
        'No NSIS installer hook is configured, so the uninstaller has no data-removal branch.'
    $hookPath = Join-Path (Join-Path $repositoryRoot 'apps\desktop\src-tauri') ($dataRemovalHook -replace '^\./', '')
    Assert-True (Test-Path -LiteralPath $hookPath) "The configured installer hook '$dataRemovalHook' does not exist."
    $started = [DateTime]::UtcNow
    $uninstallProcess = Start-Process -FilePath $uninstaller -ArgumentList '/S' -PassThru
    $uninstallProcess.WaitForExit(120000) | Out-Null
    $deadline = [DateTime]::UtcNow.AddSeconds(60)
    do {
        Start-Sleep -Seconds 2
        $settled = (-not (Test-Path -LiteralPath $installDirectory)) -and
            ((Get-FetchpathUninstallEntries).Count -eq 0)
    } while (-not $settled -and [DateTime]::UtcNow -lt $deadline)
    $observation.uninstall = [ordered]@{
        exitCode = $uninstallProcess.ExitCode
        elapsedSeconds = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 1)
        installDirectoryRemains = (Test-Path -LiteralPath $installDirectory)
        remainingInstalledFiles = Get-InstalledTree $installDirectory
        startMenuShortcutRemains = (Test-Path -LiteralPath $startMenuShortcut)
        desktopShortcutRemains = (Test-Path -LiteralPath $desktopShortcut)
        uninstallEntriesRemain = Get-FetchpathUninstallEntries
        appDataDirectoryRemains = (Test-Path -LiteralPath $appDataDirectory)
        queueFileRemains = (Test-Path -LiteralPath $queuePath)
        instanceLockRemains = (Test-Path -LiteralPath (Join-Path $appDataDirectory 'instance.lock'))
        localAppDataBundleDirectoryRemains = (Test-Path -LiteralPath $localAppDataBundleDirectory)
        localAppDataBundleContents = if (Test-Path -LiteralPath $localAppDataBundleDirectory) {
            @(Get-ChildItem -LiteralPath $localAppDataBundleDirectory -Force | ForEach-Object { $_.Name })
        } else { @() }
        installPathRegistryKeyRemains = (Test-Path -LiteralPath $installPathKey)
        installPathRegistryValueAfterUninstall = if (Test-Path -LiteralPath $installPathKey) {
            (Get-ItemProperty -LiteralPath $installPathKey).'(default)'
        } else { $null }
        # FP-030 added a real data-removal branch. This is read from the
        # shipped configuration rather than asserted as a constant: the
        # previous version of this field hardcoded $true for a control the
        # uninstaller did not actually have.
        dataRemovalHook = $dataRemovalHook
        dataRemovalPrompt = 'Interactive uninstall shows a Yes/No message box defaulting to No; a silent uninstall keeps the data.'
    }
    $installed = $observation.uninstall.installDirectoryRemains
    Assert-True ($observation.uninstall.exitCode -eq 0) "The uninstaller exited with code $($observation.uninstall.exitCode)."
    Assert-True (@($observation.uninstall.remainingInstalledFiles).Count -eq 0) `
        "Uninstall left program files behind: $(@($observation.uninstall.remainingInstalledFiles) -join ', ')"
    Assert-True (-not $observation.uninstall.startMenuShortcutRemains) 'Uninstall left the Start menu shortcut behind.'
    Assert-True (-not $observation.uninstall.desktopShortcutRemains) 'Uninstall left the desktop shortcut behind.'
    Assert-True (@($observation.uninstall.uninstallEntriesRemain).Count -eq 0) 'Uninstall left an Apps & features entry behind.'
    # A silent uninstall takes the keeping branch of NSIS_HOOK_PREUNINSTALL,
    # because there is nobody present to answer and keeping is recoverable.
    # These assert the documented behaviour rather than merely observing it.
    Assert-True $observation.uninstall.appDataDirectoryRemains `
        'A silent uninstall unexpectedly deleted the application data directory.'
    Assert-True $observation.uninstall.installPathRegistryKeyRemains `
        'A silent uninstall unexpectedly deleted the recorded install path key.'
    # %LOCALAPPDATA%pp.fetchpath.desktop\EBWebView is the WebView2 profile.
    # It holds the webview's own cache and storage and is removed by the same
    # branch as the app data directory, so a silent uninstall keeps it too.
    Assert-True $observation.uninstall.localAppDataBundleDirectoryRemains `
        'A silent uninstall unexpectedly deleted the WebView2 profile directory.'

    $observation.failures = $failures
    $observation.passed = ($failures.Count -eq 0)
} catch {
    # Keep the partial observation: a crash halfway through is still evidence,
    # and the cleanup in `finally` still runs.
    $observation.abortedWith = $_.Exception.Message
    $failures.Add("Run aborted: $($_.Exception.Message)")
    $observation.failures = $failures
    $observation.passed = $false
} finally {
    foreach ($candidate in @($seedProcess, $verifyProcess)) {
        if ($candidate) {
            $candidate.Refresh()
            if (-not $candidate.HasExited) { Stop-Process -Id $candidate.Id -Force }
        }
    }
    foreach ($stray in @(Get-Process -Name 'Fetchpath' -ErrorAction SilentlyContinue)) {
        Stop-Process -Id $stray.Id -Force
    }
    Stop-FixtureServer $fixture

    # Leave the machine as it was found.
    $cleanup = [ordered]@{}
    if ($installed -and (Test-Path -LiteralPath (Join-Path $expectedInstallDirectory 'uninstall.exe'))) {
        $forced = Start-Process -FilePath (Join-Path $expectedInstallDirectory 'uninstall.exe') -ArgumentList '/S' -PassThru
        $forced.WaitForExit(120000) | Out-Null
        $forcedDeadline = [DateTime]::UtcNow.AddSeconds(60)
        while ((Test-Path -LiteralPath $expectedInstallDirectory) -and [DateTime]::UtcNow -lt $forcedDeadline) {
            Start-Sleep -Seconds 2
        }
        $cleanup.forcedUninstall = $true
    }
    if (-not $observation.preflight.localAppDataBundleDirectoryExistedBefore -and
        (Test-Path -LiteralPath $localAppDataBundleDirectory)) {
        Remove-Item -LiteralPath $localAppDataBundleDirectory -Recurse -Force
        $cleanup.webviewProfileCreatedByThisRunRemoved = $true
    }
    if (-not $observation.preflight.manufacturerRegistryKeyExistedBefore -and (Test-Path -LiteralPath $manufacturerKey)) {
        Remove-Item -LiteralPath $manufacturerKey -Recurse -Force
        $cleanup.manufacturerRegistryKeyCreatedByThisRunRemoved = $true
    }
    if ($restoreAppData) {
        if (Test-Path -LiteralPath $appDataDirectory) { Remove-Item -LiteralPath $appDataDirectory -Recurse -Force }
        Copy-Item -LiteralPath (Join-Path $backupDirectory 'app.fetchpath.desktop') -Destination $env:APPDATA -Recurse -Force
        $cleanup.appDataRestoredFromBackup = $true
    } elseif (Test-Path -LiteralPath $appDataDirectory) {
        Remove-Item -LiteralPath $appDataDirectory -Recurse -Force
        $cleanup.appDataCreatedByThisRunRemoved = $true
    }
    $cleanup.appDataPresentAtExit = (Test-Path -LiteralPath $appDataDirectory)
    $cleanup.installDirectoryPresentAtExit = (Test-Path -LiteralPath $expectedInstallDirectory)
    $cleanup.uninstallEntriesAtExit = (Get-FetchpathUninstallEntries).Count
    $cleanup.manufacturerRegistryKeyPresentAtExit = (Test-Path -LiteralPath $manufacturerKey)
    $cleanup.webviewProfilePresentAtExit = (Test-Path -LiteralPath $localAppDataBundleDirectory)
    $cleanup.matchesPreflight = ($cleanup.appDataPresentAtExit -eq $appDataExistedBefore) -and
        (-not $cleanup.installDirectoryPresentAtExit) -and
        ($cleanup.uninstallEntriesAtExit -eq 0) -and
        ($cleanup.manufacturerRegistryKeyPresentAtExit -eq $observation.preflight.manufacturerRegistryKeyExistedBefore) -and
        ($cleanup.webviewProfilePresentAtExit -eq $observation.preflight.localAppDataBundleDirectoryExistedBefore)
    $observation.machineRestored = $cleanup
}

[System.IO.Directory]::CreateDirectory([System.IO.Path]::GetDirectoryName($OutputPath)) | Out-Null
$json = $observation | ConvertTo-Json -Depth 8
# BOM-less UTF-8: Windows PowerShell's -Encoding utf8 writes a byte order mark,
# which breaks a plain JSON parse of the evidence file.
[System.IO.File]::WriteAllText($OutputPath, $json, [System.Text.UTF8Encoding]::new($false))
$json

if ($failures.Count -gt 0) {
    throw "Packaging lifecycle checks failed:`n - $($failures -join "`n - ")"
}
