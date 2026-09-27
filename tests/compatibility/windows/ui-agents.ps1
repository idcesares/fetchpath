# Fetchpath desktop: agents' requests and agents' access (FP-066, A01, A13).
#
# Drives the real optimized executable through Windows UI Automation, in its
# own data folder (FETCHPATH_APP_DATA_DIR), so the person's queue is never
# touched. Two agent requests are made the way an agent makes them, through
# `fetchpath mcp`, into a folder the agent was not given, and then:
#
#   1. each waiting request shows in the queue with its agent and reason, and
#      Approve and Deny buttons named for the download;
#   2. the request reaches the assertive live region;
#   3. Approve, pressed from the keyboard, lets one download finish; Deny ends
#      the other cancelled with nothing saved;
#   4. in Settings, every keyboard-focusable control of the AI agents section
#      has a name; an agent added by name shows its limits and actions; a
#      folder granted from the command line shows with a named Remove button;
#      Save limits, Remove and Revoke access change what the engine holds, as
#      `fetchpath agents --json` reports it.
#
# Keyboard input goes to the renderer window as in ui-accessibility.ps1.
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
$workDirectory = Join-Path $repositoryRoot 'work\fp066-agents'
if (-not $OutputPath) { $OutputPath = Join-Path $workDirectory 'ui-agents.json' }
if (Test-Path -LiteralPath $workDirectory) { Remove-Item -LiteralPath $workDirectory -Recurse -Force }
$dataDirectory = Join-Path $workDirectory 'data'
$requestsDirectory = Join-Path $workDirectory 'requests'
$grantedDirectory = Join-Path $workDirectory 'granted'
foreach ($folder in @($dataDirectory, $requestsDirectory, $grantedDirectory)) {
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

function Wait-ByName($Root, [string] $Name, [int] $TimeoutSeconds = 20) {
    return Wait-ForCondition { Find-ByName $Root $Name } $TimeoutSeconds "a control named '$Name'"
}

function Invoke-Element($Element) {
    $Element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    Start-Sleep -Milliseconds 500
}

# Focus a control and press Enter on it, as a keyboard user would.
function Press-Element($Element) {
    [FetchpathUia]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
    $Element.SetFocus()
    Start-Sleep -Milliseconds 250
    [FetchpathUia]::PostKey($renderWidget, $VK.Enter)
    Start-Sleep -Milliseconds 700
}

function Get-Agents {
    $json = & $cliPath agents --json
    if ($LASTEXITCODE -ne 0) { throw "fetchpath agents failed: $json" }
    return ($json | ConvertFrom-Json).policies
}

# Makes requests the way an agent host does: JSON-RPC over the standard
# input and output of `fetchpath mcp`.
function Send-AgentRequests([string[]] $Names) {
    $start = [System.Diagnostics.ProcessStartInfo]::new($cliPath, 'mcp --agent harness')
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.UseShellExecute = $false
    $start.Environment['FETCHPATH_APP_DATA_DIR'] = $dataDirectory
    $server = [System.Diagnostics.Process]::Start($start)
    $id = 0
    $call = {
        param($Method, $Params)
        $script:id++
        $message = @{ jsonrpc = '2.0'; id = $script:id; method = $Method; params = $Params } | ConvertTo-Json -Depth 8 -Compress
        $server.StandardInput.WriteLine($message)
        $server.StandardInput.Flush()
        while ($true) {
            $line = $server.StandardOutput.ReadLine()
            if ($null -eq $line) { throw 'fetchpath mcp closed its output.' }
            $reply = $line | ConvertFrom-Json
            if ($reply.PSObject.Properties['id'] -and $reply.id -eq $script:id) { return $reply }
        }
    }
    $script:id = 0
    & $call 'initialize' @{ protocolVersion = '2025-06-18'; capabilities = @{}; clientInfo = @{ name = 'ui-agents'; version = '1' } } | Out-Null
    $server.StandardInput.WriteLine('{"jsonrpc":"2.0","method":"notifications/initialized"}')
    $states = foreach ($name in $Names) {
        $reply = & $call 'tools/call' @{ name = 'download'; arguments = @{ url = "$($fixture.BaseUrl)/$name"; folder = $requestsDirectory; kind = 'file' } }
        $reply.result.structuredContent.state
    }
    $server.StandardInput.Close()
    $server.WaitForExit(10000) | Out-Null
    return $states
}

try {
    $fixture = Start-FixtureServer -Size (512 * 1024)
    $observation.requestStates = @(Send-AgentRequests @('approve-me.bin', 'deny-me.bin'))
    Assert-True (@($observation.requestStates | Where-Object { $_ -ne 'awaiting_approval' }).Count -eq 0) `
        "Agent requests outside a grant did not wait for approval: $($observation.requestStates -join ', ')"

    $process = Start-Process -FilePath $ApplicationPath -PassThru
    $renderer = Get-RendererRoot $process
    $renderWidget = Get-RenderWidgetHandle $process

    # 1. The waiting requests, their agent, reason and named actions.
    $approve = Wait-ByName $renderer 'Approve: approve-me.bin'
    $deny = Wait-ByName $renderer 'Deny: deny-me.bin'
    $observation.approveButton = (Get-ElementInfo $approve).controlType
    $observation.denyButton = (Get-ElementInfo $deny).controlType
    $note = Find-VisibleText $renderer '^The agent harness asks to download this: it would save outside the folders you let it use\.$'
    $observation.approvalNote = $note
    Assert-True ([bool] $note) 'The waiting request did not name its agent and reason.'
    Assert-True ((Get-ControlText $renderer 'live-alert') -match '2 agent requests wait for your approval') `
        'The waiting requests did not reach the assertive live region.'
    $observation.liveAlert = Get-ControlText $renderer 'live-alert'

    # 3. Approve from the keyboard; Deny.
    Press-Element $approve
    $saved = Join-Path $requestsDirectory 'approve-me.bin'
    Wait-ForCondition { Test-Path -LiteralPath $saved } 40 'the approved download to be saved' | Out-Null
    $observation.approvedSavedBytes = (Get-Item -LiteralPath $saved).Length
    Assert-True ($observation.approvedSavedBytes -eq 512 * 1024) 'The approved download was not saved whole.'
    Press-Element (Find-ByName $renderer 'Deny: deny-me.bin')
    Wait-ForCondition { -not (Find-ByName $renderer 'Deny: deny-me.bin') } 20 'the denied request to leave the waiting state' | Out-Null
    $observation.deniedSaved = Test-Path -LiteralPath (Join-Path $requestsDirectory 'deny-me.bin')
    Assert-True (-not $observation.deniedSaved) 'A denied request saved a file.'
    $deniedState = (& $cliPath ls --json | ConvertFrom-Json).jobs |
        Where-Object { $_.destination -like '*deny-me.bin' } | Select-Object -ExpandProperty state
    $observation.deniedState = $deniedState
    Assert-True ($deniedState -eq 'cancelled') "The denied request is $deniedState, not cancelled."

    # 4. Settings: the AI agents section.
    Press-Element (Find-ById $renderer 'open-settings')
    $newName = Wait-ForCondition { Find-ById $renderer 'agent-new-name' } 20 'the agent name field'
    $observation.agentNameField = (Get-ElementInfo $newName).name
    Assert-True ($observation.agentNameField -eq 'Add an agent by the name it uses') `
        "The agent name field is named '$($observation.agentNameField)'."
    Press-Element $newName
    [FetchpathUia]::PostText($renderWidget, 'harness')
    Start-Sleep -Milliseconds 400
    [FetchpathUia]::PostKey($renderWidget, $VK.Enter)
    $addFolder = Wait-ByName $renderer 'Add a folder for harness'
    Assert-True (@(Get-Agents | Where-Object { $_.agent -eq 'harness' }).Count -eq 1) 'Adding an agent did not reach the engine.'
    $observation.focusAfterAdd = (Get-RendererFocus $renderer).name
    Assert-True ($observation.focusAfterAdd -eq 'Add a folder for harness') `
        "Focus after adding an agent is on '$($observation.focusAfterAdd)'."

    # A folder granted from the command line shows up with a named Remove.
    & $cliPath agents grant harness $grantedDirectory | Out-Null
    Press-Element (Find-ById $renderer 'settings-close')
    Press-Element (Find-ById $renderer 'open-settings')
    $remove = Wait-ByName $renderer "Remove $grantedDirectory from harness"

    $section = Find-ById $renderer 'agents-group'
    $unnamed = @()
    $items = Get-DescendantElements $section
    for ($index = 0; $index -lt $items.Count; $index++) {
        $info = Get-ElementInfo $items.Item($index)
        if (-not $info -or -not $info.isKeyboardFocusable) { continue }
        if ($interactiveTypes -notcontains $info.controlType) { continue }
        if (-not $info.name) { $unnamed += "$($info.controlType) $($info.automationId)" }
    }
    $observation.unnamedAgentControls = $unnamed
    Assert-True ($unnamed.Count -eq 0) "Unnamed controls in AI agents: $($unnamed -join '; ')"
    foreach ($name in @('Largest download (MB)', 'Downloads an hour', 'Save limits for harness', 'Revoke access for harness')) {
        Assert-True ([bool] (Find-ByName $renderer $name)) "No control named '$name'."
    }

    # Save limits.
    $size = Find-ById $renderer 'agent-size-harness'
    $size.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('5')
    Press-Element (Find-ByName $renderer 'Save limits for harness')
    Wait-ForCondition { (Get-Agents | Where-Object { $_.agent -eq 'harness' }).policy.max_bytes -eq 5MB } 10 'the size limit to be saved' | Out-Null
    $observation.savedLimitBytes = (Get-Agents | Where-Object { $_.agent -eq 'harness' }).policy.max_bytes

    # Remove the folder, then revoke.
    Press-Element (Find-ByName $renderer "Remove $grantedDirectory from harness")
    Wait-ForCondition { @((Get-Agents | Where-Object { $_.agent -eq 'harness' }).policy.folders).Count -eq 0 } 10 'the folder to be removed' | Out-Null
    $observation.folderRemoved = $true
    Press-Element (Find-ByName $renderer 'Revoke access for harness')
    Wait-ForCondition { @(Get-Agents | Where-Object { $_.agent -eq 'harness' }).Count -eq 0 } 10 'the agent to be revoked' | Out-Null
    $observation.revoked = $true
    $observation.focusAfterRevoke = (Get-RendererFocus $renderer).automationId
    Assert-True ($observation.focusAfterRevoke -eq 'agent-new-name') `
        "Focus after revoking is on '$($observation.focusAfterRevoke)'."
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
    $json = $observation | ConvertTo-Json -Depth 6
    [System.IO.File]::WriteAllText($OutputPath, $json, [System.Text.UTF8Encoding]::new($false))
}
if ($failures.Count) { throw "ui-agents failed:`n$($failures -join "`n")" }
Write-Output "ui-agents passed. Observation: $OutputPath"
