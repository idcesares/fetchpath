# Fetchpath desktop: paired computers and LAN sharing (FP-033, A01, A07, A10).
#
# Two data folders stand in for two computers on loopback: A is the release
# desktop and its engine, B is a second engine driven by the command line.
# Through Windows UI Automation, from the keyboard:
#
#   1. sharing starts off;
#   2. A shows a pairing code with its fingerprint; B joins with it; A then
#      shows B's fingerprint and B shows A's;
#   3. with sharing on, B gets a checksum-verified file from A with the link
#      dead, and is refused one A downloaded from a signed link;
#   4. Remove unpairs B, and B is refused from then on; sharing turns off;
#   5. every control in the section has a name.
#
# Binds to 127.0.0.1 (FETCHPATH_LAN_BIND) so no firewall prompt appears.
# Writes a JSON observation to -OutputPath and throws on any failure.

[CmdletBinding()]
param(
    [string] $ApplicationPath,
    [string] $OutputPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'uia-common.ps1')

$repositoryRoot = Get-RepositoryRoot $PSScriptRoot
if (-not $ApplicationPath) {
    $ApplicationPath = Join-Path $repositoryRoot 'target\release\fetchpath-desktop.exe'
}
$ApplicationPath = [System.IO.Path]::GetFullPath($ApplicationPath)
$cliPath = Join-Path (Split-Path $ApplicationPath) 'fetchpath.exe'
foreach ($path in @($ApplicationPath, $cliPath)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Not found: $path" }
}
$workDirectory = Join-Path $repositoryRoot 'work\fp033-lan'
if (-not $OutputPath) { $OutputPath = Join-Path $workDirectory 'ui-lan.json' }
if (Test-Path -LiteralPath $workDirectory) { Remove-Item -LiteralPath $workDirectory -Recurse -Force }
$aData = Join-Path $workDirectory 'a'
$bData = Join-Path $workDirectory 'b'
$aDownloads = Join-Path $workDirectory 'a-downloads'
$bDownloads = Join-Path $workDirectory 'b-downloads'
foreach ($folder in @($aData, $bData, $aDownloads, $bDownloads)) {
    [System.IO.Directory]::CreateDirectory($folder) | Out-Null
}
$env:FETCHPATH_LAN_BIND = '127.0.0.1'
Remove-Item Env:FETCHPATH_DATA_DIR -ErrorAction SilentlyContinue

$VK = Get-VirtualKeys
$failures = [System.Collections.Generic.List[string]]::new()
function Assert-True([bool] $Condition, [string] $Message) {
    if (-not $Condition) { $failures.Add($Message) }
}
$interactiveTypes = @('Button', 'Edit', 'ComboBox', 'CheckBox', 'RadioButton', 'Hyperlink', 'Slider', 'Spinner')
$observation = [ordered]@{ application = $ApplicationPath }
$process = $null
$fixtures = @()

# Runs the command line against computer A or B.
function Invoke-Cli([string] $Computer, [string[]] $Arguments) {
    $env:FETCHPATH_APP_DATA_DIR = if ($Computer -eq 'A') { $aData } else { $bData }
    $output = & $cliPath @Arguments 2>&1 | Out-String
    return [pscustomobject]@{ exitCode = $LASTEXITCODE; output = $output.Trim() }
}

function Get-Lan([string] $Computer) {
    $result = Invoke-Cli $Computer @('lan', '--json')
    if ($result.exitCode -ne 0) { throw "fetchpath lan failed on ${Computer}: $($result.output)" }
    return ($result.output | ConvertFrom-Json).lan
}

function Get-Sha256([int] $Size) {
    $body = [byte[]]::new($Size)
    for ($offset = 0; $offset -lt $Size; $offset++) { $body[$offset] = [byte] 0x5A }
    return ([System.BitConverter]::ToString([System.Security.Cryptography.SHA256]::Create().ComputeHash($body)) -replace '-', '').ToLowerInvariant()
}

# Asks A for a file through B's command line, with a link nothing serves.
function Request-FromA([string] $Sha256, [int] $Size, [string] $Name) {
    $key = (Get-Lan 'B').devices[0].key
    $destination = Join-Path $bDownloads $Name
    $result = Invoke-Cli 'B' @('fetch-verified', '--sha256', $Sha256, '--size', "$Size",
        '--peer', "127.0.0.1:47631=$key", "http://127.0.0.1:$(Get-ClosedPort)/$Name", $destination)
    return [ordered]@{
        exitCode = $result.exitCode
        source = if ($result.exitCode -eq 0) { ($result.output | ConvertFrom-Json).source } else { $null }
        saved = (Test-Path -LiteralPath $destination)
    }
}

# A paragraph keeps no automation id, so it is found by its text.
function Find-VisibleText($Root, [string] $Pattern) {
    $items = Get-DescendantElements $Root
    for ($index = 0; $index -lt $items.Count; $index++) {
        $info = Get-ElementInfo $items.Item($index)
        if ($info -and $info.name -and $info.name -match $Pattern) { return $info.name }
    }
    return $null
}

function Find-ByName($Root, [string] $Name) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name
    )
    return $Root.FindFirst([System.Windows.Automation.TreeScope]::Subtree, $condition)
}

# Focus, then a key, as a keyboard user operates the control.
function Press-Element($Element, [byte] $Key) {
    [FetchpathUia]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
    $Element.SetFocus()
    Start-Sleep -Milliseconds 250
    [FetchpathUia]::PostKey($renderWidget, $Key)
    Start-Sleep -Milliseconds 700
}

try {
    $null = Invoke-Cli 'A' @('settings', 'onboarding-completed', 'on')
    $env:FETCHPATH_APP_DATA_DIR = $aData
    $process = Start-Process -FilePath $ApplicationPath -PassThru
    $renderer = Get-RendererRoot $process
    $renderWidget = Get-RenderWidgetHandle $process
    Wait-ForCondition { Find-ById $renderer 'open-settings' } 30 'the window to load' | Out-Null
    Open-Popup $renderer 'open-settings'
    Wait-ForCondition { Find-ById $renderer 'lan-group' } 20 'the Paired computers section' | Out-Null

    # 1. Off to begin with.
    $observation.sharingAtStart = Wait-ForCondition {
        $text = Get-ControlText $renderer 'lan-sharing-state'
        if ($text) { $text }
    } 10 'the sharing state'
    Assert-True ($observation.sharingAtStart -match '^Off\.') "Sharing did not start off: $($observation.sharingAtStart)"
    $aFingerprint = (Get-Lan 'A').fingerprint
    $bFingerprint = (Get-Lan 'B').fingerprint
    $observation.ownFingerprintShown = Wait-ForCondition { Find-VisibleText $renderer "^This computer's fingerprint: " } 10 'the fingerprint'
    Assert-True ($observation.ownFingerprintShown -match [regex]::Escape($aFingerprint)) 'A does not show its own fingerprint.'

    # 2. Pair.
    Press-Element (Find-ById $renderer 'lan-pair') $VK.Enter
    $code = Wait-ForCondition { Find-VisibleText $renderer '^[0-9A-Z]{5}-[0-9A-Z]{5}$' } 10 'a pairing code'
    $instructions = Find-VisibleText $renderer '^On the other computer'
    $observation.pairingInstructions = $instructions -replace [regex]::Escape($code), '<code>'
    Assert-True ($instructions -match [regex]::Escape($aFingerprint)) 'The code is shown without this computer''s fingerprint.'
    $address = [regex]::Match($instructions, '127\.0\.0\.1:\d+').Value
    $joined = Invoke-Cli 'B' @('lan', 'join', $address, $code, 'Desk', '--json')
    Assert-True ($joined.exitCode -eq 0) "B could not join: $($joined.output)"
    $observation.bSawFingerprint = ($joined.output | ConvertFrom-Json).device.fingerprint
    Assert-True ($observation.bSawFingerprint -eq $aFingerprint) 'B saw another fingerprint for A.'
    $observation.afterPairing = Wait-ForCondition { Find-VisibleText $renderer '^Paired with a computer' } 15 'A to show the pairing'
    Assert-True ($observation.afterPairing -match [regex]::Escape($bFingerprint)) 'A does not show B''s fingerprint.'
    $observation.codeHiddenAfterUse = -not (Find-VisibleText $renderer '^[0-9A-Z]{5}-[0-9A-Z]{5}$')
    Assert-True $observation.codeHiddenAfterUse 'The used code is still shown.'
    $null = Wait-ForCondition { Find-ByName $renderer "Remove paired device, fingerprint $bFingerprint" } 10 'a named Remove button'

    # 3. Sharing on, one public and one signed-link download on A.
    Press-Element (Find-ById $renderer 'lan-sharing') $VK.Space
    $observation.sharingOn = Wait-ForCondition {
        $text = Get-ControlText $renderer 'lan-sharing-state'
        if ($text -match '^On\. Paired computers reach this one at') { $text }
    } 10 'sharing to start'
    $publicSize = 256 * 1024
    $signedSize = 128 * 1024
    $publicSha = Get-Sha256 $publicSize
    $signedSha = Get-Sha256 $signedSize
    $public = Start-FixtureServer -Size $publicSize
    $signed = Start-FixtureServer -Size $signedSize
    $fixtures = @($public, $signed)
    $null = Invoke-Cli 'A' @('add', "$($public.BaseUrl)/public.bin", '--to', $aDownloads, '--sha256', $publicSha, '--wait')
    $null = Invoke-Cli 'A' @('add', "$($signed.BaseUrl)/signed.bin?sig=secret", '--to', $aDownloads, '--sha256', $signedSha, '--wait')
    Wait-ForCondition { $r = Invoke-Cli 'A' @('cache', '--json'); ($r.output | ConvertFrom-Json).cache.entries -eq 2 } 10 'both files in A''s cache' | Out-Null
    foreach ($fixture in $fixtures) { Stop-FixtureServer $fixture }
    $fixtures = @()
    $observation.publicFromA = Request-FromA $publicSha $publicSize 'public.bin'
    Assert-True ($observation.publicFromA.source -eq 'peer') "B did not get the public file from A: $($observation.publicFromA | ConvertTo-Json -Compress)"
    $observation.signedFromA = Request-FromA $signedSha $signedSize 'signed.bin'
    Assert-True (-not $observation.signedFromA.saved) 'A offered a file it downloaded from a signed link.'

    # 4. Remove, then off.
    # Found again: the list is redrawn whenever the state changes.
    Press-Element (Find-ByName $renderer "Remove paired device, fingerprint $bFingerprint") $VK.Enter
    Wait-ForCondition { @((Get-Lan 'A').devices).Count -eq 0 } 10 'B to be unpaired' | Out-Null
    $observation.afterUnpair = Request-FromA $publicSha $publicSize 'after-unpair.bin'
    Assert-True (-not $observation.afterUnpair.saved) 'An unpaired computer still got a file.'
    Press-Element (Find-ById $renderer 'lan-sharing') $VK.Space
    $observation.sharingOff = Wait-ForCondition {
        $text = Get-ControlText $renderer 'lan-sharing-state'
        if ($text -match '^Off\.') { $text }
    } 10 'sharing to stop'

    # 5. Names.
    $unnamed = @()
    $items = Get-DescendantElements (Find-ById $renderer 'lan-group')
    for ($index = 0; $index -lt $items.Count; $index++) {
        $info = Get-ElementInfo $items.Item($index)
        if (-not $info -or -not $info.isKeyboardFocusable) { continue }
        if ($interactiveTypes -notcontains $info.controlType) { continue }
        if (-not $info.name) { $unnamed += "$($info.controlType) $($info.automationId)" }
    }
    $observation.unnamedLanControls = $unnamed
    Assert-True ($unnamed.Count -eq 0) "Unnamed controls in Paired computers: $($unnamed -join '; ')"
} catch {
    $failures.Add("Run stopped: $($_.Exception.Message)")
} finally {
    if ($process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force
        $process.WaitForExit(20000) | Out-Null
    }
    foreach ($computer in @('A', 'B')) { $null = Invoke-Cli $computer @('engine', 'stop') }
    foreach ($fixture in $fixtures) { Stop-FixtureServer $fixture }
    $observation.failures = @($failures)
    $observation.passed = $failures.Count -eq 0
    $json = $observation | ConvertTo-Json -Depth 8
    [System.IO.File]::WriteAllText($OutputPath, $json, [System.Text.UTF8Encoding]::new($false))
}
if ($failures.Count) { throw "ui-lan failed:`n$($failures -join "`n")" }
Write-Output "ui-lan passed. Observation: $OutputPath"
