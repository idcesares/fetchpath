# Fetchpath desktop: smart rules in Settings, Add download and browser
# captures (FP-075, A01, A07).
#
# Drives the real optimized executable through Windows UI Automation, in its
# own data folder (FETCHPATH_APP_DATA_DIR), so the person's queue is never
# touched:
#
#   1. Settings adds a rule (.bin files into a folder) through the engine, as
#      `fetchpath rules --json` reports it; every focusable control in the
#      Rules section has a name, and the rule has a named Remove button;
#   2. testing a link in Settings says which rule decides;
#   3. a link pasted into Add download names the rule and why, proposes its
#      folder, and is saved there;
#   4. a capture sent through the real browser host is saved there too;
#   5. Remove takes the rule out of the engine.
#
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
$binDirectory = Split-Path $ApplicationPath
$cliPath = Join-Path $binDirectory 'fetchpath.exe'
$hostPath = Join-Path $binDirectory 'fetchpath-browser-host.exe'
foreach ($path in @($ApplicationPath, $cliPath, $hostPath)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Not found: $path" }
}
$workDirectory = Join-Path $repositoryRoot 'work\fp075-rules'
if (-not $OutputPath) { $OutputPath = Join-Path $workDirectory 'ui-rules.json' }
if (Test-Path -LiteralPath $workDirectory) { Remove-Item -LiteralPath $workDirectory -Recurse -Force }
$dataDirectory = Join-Path $workDirectory 'data'
$defaultDirectory = Join-Path $workDirectory 'default'
$ruleDirectory = Join-Path $workDirectory 'ruled'
foreach ($folder in @($dataDirectory, $defaultDirectory, $ruleDirectory)) {
    [System.IO.Directory]::CreateDirectory($folder) | Out-Null
}
$env:FETCHPATH_APP_DATA_DIR = $dataDirectory

$VK = Get-VirtualKeys
$failures = [System.Collections.Generic.List[string]]::new()
function Assert-True([bool] $Condition, [string] $Message) {
    if (-not $Condition) { $failures.Add($Message) }
}
$interactiveTypes = @('Button', 'Edit', 'ComboBox', 'CheckBox', 'RadioButton', 'Hyperlink', 'Slider', 'Spinner')
$observation = [ordered]@{ application = $ApplicationPath }
$process = $null
$fixture = $null
$size = 256 * 1024

function Find-ByName($Root, [string] $Name) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name
    )
    return $Root.FindFirst([System.Windows.Automation.TreeScope]::Subtree, $condition)
}

function Find-VisibleText($Root, [string] $Pattern) {
    $items = Get-DescendantElements $Root
    for ($index = 0; $index -lt $items.Count; $index++) {
        $info = Get-ElementInfo $items.Item($index)
        if ($info -and $info.name -and $info.name -match $Pattern) { return $info.name }
    }
    return $null
}

function Press-Element($Element) {
    [FetchpathUia]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
    $Element.SetFocus()
    Start-Sleep -Milliseconds 250
    [FetchpathUia]::PostKey($renderWidget, $VK.Enter)
    Start-Sleep -Milliseconds 700
}

function Get-Rules {
    $json = & $cliPath rules --json
    if ($LASTEXITCODE -ne 0) { throw "fetchpath rules failed: $json" }
    return @(($json | ConvertFrom-Json).rules)
}

# One native-messaging exchange with the browser host, as Chrome makes it.
function Send-Capture([string] $Url, [string] $Name) {
    $request = @{
        schema_version = 1; type = 'capture'; capture_id = [guid]::NewGuid().ToString()
        method = 'GET'; url = $Url; suggested_filename = $Name; referrer = $null
        cookies = @(); user_initiated = $true
    } | ConvertTo-Json -Compress
    $body = [System.Text.Encoding]::UTF8.GetBytes($request)
    $start = [System.Diagnostics.ProcessStartInfo]::new($hostPath, 'chrome-extension://lfikhkjdpjcjaboanknaabncpkbgoele/')
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.UseShellExecute = $false
    $start.Environment['FETCHPATH_APP_DATA_DIR'] = $dataDirectory
    $bridge = [System.Diagnostics.Process]::Start($start)
    $stream = $bridge.StandardInput.BaseStream
    $stream.Write([BitConverter]::GetBytes([uint32] $body.Length), 0, 4)
    $stream.Write($body, 0, $body.Length)
    $stream.Close()
    $reply = $bridge.StandardOutput.ReadToEnd()
    $bridge.WaitForExit(10000) | Out-Null
    return $reply.Substring([Math]::Min(4, $reply.Length))
}

try {
    $fixture = Start-FixtureServer -Size $size
    & $cliPath settings default-destination-dir $defaultDirectory | Out-Null
    # A returning person: the first-run guide is covered by ui-accessibility.
    & $cliPath settings onboarding-completed on | Out-Null

    $process = Start-Process -FilePath $ApplicationPath -PassThru
    $renderer = Get-RendererRoot $process
    $renderWidget = Get-RenderWidgetHandle $process

    # 1. Add a rule in Settings.
    Wait-ForCondition { Find-ById $renderer 'open-settings' } 30 'the window to load' | Out-Null
    Open-Popup $renderer 'open-settings'
    Wait-ForCondition { Find-ById $renderer 'rules-group' } 20 'the Rules section' | Out-Null
    Press-Element (Wait-ForCondition { Find-ByName $renderer 'Add a rule' } 10 'the Add a rule toggle')
    Wait-ForCondition { Find-ById $renderer 'rule-types' } 10 'the rule form' | Out-Null
    Set-Field $renderer 'rule-name' 'Harness'
    Set-Field $renderer 'rule-types' 'bin'
    Set-Field $renderer 'rule-folder' $ruleDirectory
    Press-Element (Find-ByName $renderer 'Add rule')
    Wait-ForCondition { @(Get-Rules).Count -eq 1 } 10 'the rule to reach the engine' | Out-Null
    $observation.rules = Get-Rules
    Assert-True ((Get-Rules)[0].then.folder -eq $ruleDirectory) 'The rule saved another folder.'
    $remove = Wait-ForCondition { Find-ByName $renderer 'Remove Rule 1 (Harness)' } 10 'a named Remove button'

    $unnamed = @()
    $items = Get-DescendantElements (Find-ById $renderer 'rules-group')
    for ($index = 0; $index -lt $items.Count; $index++) {
        $info = Get-ElementInfo $items.Item($index)
        if (-not $info -or -not $info.isKeyboardFocusable) { continue }
        if ($interactiveTypes -notcontains $info.controlType) { continue }
        if (-not $info.name) { $unnamed += "$($info.controlType) $($info.automationId)" }
    }
    $observation.unnamedRuleControls = $unnamed
    Assert-True ($unnamed.Count -eq 0) "Unnamed controls in Rules: $($unnamed -join '; ')"

    # 2. Test a link.
    Set-Field $renderer 'rule-test-link' "$($fixture.BaseUrl)/tested.bin"
    Invoke-Control $renderer 'rule-test' $process
    $tested = Wait-ForCondition {
        $text = Get-ControlText $renderer 'rule-test-result'
        if ($text -match 'Rule 1 \(Harness\) decides') { $text }
    } 20 'the test result'
    $observation.testResult = $tested
    Invoke-Control $renderer 'settings-close' $process
    Wait-ForCondition { -not (Find-ById $renderer 'rules-group') } 10 'Settings to close' | Out-Null

    # 3. Add download names the rule, proposes its folder and saves there.
    # From the keyboard, as ui-accessibility does: Enter on Add download
    # lands in the links box, and the link is typed so the dialog analyses it.
    $null = Set-ControlFocus $renderer 'add-open' $process
    Start-Sleep -Milliseconds 300
    [FetchpathUia]::PostKey($renderWidget, $VK.Enter)
    Wait-ForCondition { Find-ById $renderer 'url' } 10 'Add download to open' | Out-Null
    Start-Sleep -Milliseconds 400
    [FetchpathUia]::PostText($renderWidget, "$($fixture.BaseUrl)/pasted.bin")
    # A paragraph keeps no automation id, so the note is found by its text.
    $note = Wait-ForCondition { Find-VisibleText $renderer '^Rule 1 \(Harness\)' } 20 'the rule note in Add download'
    $observation.ruleNote = $note
    Assert-True ($note -match [regex]::Escape("saves in $ruleDirectory")) "The note did not name the folder: $note"
    $observation.proposedDestination = Get-FieldValue $renderer 'destination'
    Assert-True ($observation.proposedDestination -like "$ruleDirectory\*") `
        "Add download proposed $($observation.proposedDestination)."
    Press-Element (Find-ById $renderer 'start-download')
    $pasted = Join-Path $ruleDirectory 'pasted.bin'
    Wait-ForCondition { (Test-Path -LiteralPath $pasted) -and (Get-Item -LiteralPath $pasted).Length -eq $size } 40 'the pasted link to be saved in the rule folder' | Out-Null
    $observation.pastedSaved = $pasted

    # 4. A browser capture follows the rule; one the rule does not match
    # goes to the default folder.
    $observation.captureReply = Send-Capture "$($fixture.BaseUrl)/captured.bin" 'captured.bin'
    Send-Capture "$($fixture.BaseUrl)/other.dat" 'other.dat' | Out-Null
    $captured = Join-Path $ruleDirectory 'captured.bin'
    $other = Join-Path $defaultDirectory 'other.dat'
    Wait-ForCondition { (Test-Path -LiteralPath $captured) -and (Test-Path -LiteralPath $other) } 40 'the captures to be saved' | Out-Null
    $observation.capturedSaved = $captured
    $observation.unmatchedSaved = $other

    # 5. Remove.
    Open-Popup $renderer 'open-settings'
    Press-Element (Wait-ForCondition { Find-ByName $renderer 'Remove Rule 1 (Harness)' } 10 'the Remove button')
    Wait-ForCondition { @(Get-Rules).Count -eq 0 } 10 'the rule to be removed' | Out-Null
    $observation.removed = $true
} catch {
    $failures.Add("Run stopped: $($_.Exception.Message)")
} finally {
    if ($process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force
        $process.WaitForExit(20000) | Out-Null
    }
    if (Test-Path -LiteralPath $cliPath) { & $cliPath engine stop | Out-Null }
    if ($fixture) { Stop-FixtureServer $fixture }
    $observation.failures = @($failures)
    $observation.passed = $failures.Count -eq 0
    $json = $observation | ConvertTo-Json -Depth 8
    [System.IO.File]::WriteAllText($OutputPath, $json, [System.Text.UTF8Encoding]::new($false))
}
if ($failures.Count) { throw "ui-rules failed:`n$($failures -join "`n")" }
Write-Output "ui-rules passed. Observation: $OutputPath"
