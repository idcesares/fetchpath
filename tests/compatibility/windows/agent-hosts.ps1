# FP-102: isolated registration lifecycle and optional real-host MCP acceptance.
# Requires PowerShell 7, built fetchpath.exe, and installed Codex / Claude Code.
# Default runs local lifecycle only. -RunHosts makes two paid host invocations.
# Existing file-based sign-in can be copied with -CopyLocalAuth into ignored work/;
# otherwise provide a supported host environment sign-in (for example API key).
# Never uses --mcp-config: hosts load exactly the registration made by agent-setup.
# Installer upgrade/uninstall remains a separate clean-machine/sandbox check.
# Raw host events stay in ignored work/; the exported evidence contains no auth.
# Example: pwsh -File tests/compatibility/windows/agent-hosts.ps1 -RunHosts -CopyLocalAuth
# Repeat with a new WorkDirectory to avoid resuming jobs or registrations.
# Standard inference only: no Fast/Ultrafast option is enabled.
#Requires -Version 7.0
[CmdletBinding()]
param(
    [string] $ApplicationPath,
    [string] $WorkDirectory,
    [string] $OutputPath,
    [ValidateSet('codex', 'claude-code')] [string[]] $Hosts = @('codex', 'claude-code'),
    [switch] $RunHosts,
    [switch] $CopyLocalAuth,
    [string] $CodexModel = 'gpt-6.1-sol',
    [ValidateRange(30, 600)] [int] $HostTimeoutSeconds = 180
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
if (-not $ApplicationPath) { $ApplicationPath = Join-Path $repository 'target\release\fetchpath.exe' }
$ApplicationPath = (Resolve-Path -LiteralPath $ApplicationPath).Path
if (-not $WorkDirectory) { $WorkDirectory = Join-Path $repository ('work\fp102-agent-hosts-' + [Guid]::NewGuid().ToString('N')) }
$WorkDirectory = [IO.Path]::GetFullPath($WorkDirectory)
$scratchRoot = [IO.Path]::GetFullPath((Join-Path $repository 'work')) + [IO.Path]::DirectorySeparatorChar
if (-not $WorkDirectory.StartsWith($scratchRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'WorkDirectory must be inside the ignored repository work directory.' }
if (Test-Path -LiteralPath $WorkDirectory) { throw 'Use a fresh WorkDirectory; existing evidence and sign-in files are never overwritten.' }
[IO.Directory]::CreateDirectory($WorkDirectory) | Out-Null
if (-not $OutputPath) { $OutputPath = Join-Path $WorkDirectory 'result.json' }
$environmentNames = @('CODEX_HOME', 'CLAUDE_CONFIG_DIR', 'FETCHPATH_AGENT_SETUP_DIR', 'FETCHPATH_APP_DATA_DIR')
$savedEnvironment = @{}
foreach ($name in $environmentNames) { $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name) }
$realCodexHome = if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $env:USERPROFILE '.codex' }
$realClaudeHome = if ($env:CLAUDE_CONFIG_DIR) { $env:CLAUDE_CONFIG_DIR } else { Join-Path $env:USERPROFILE '.claude' }
$env:CODEX_HOME = Join-Path $WorkDirectory 'codex'
$env:CLAUDE_CONFIG_DIR = Join-Path $WorkDirectory 'claude'
$env:FETCHPATH_AGENT_SETUP_DIR = Join-Path $WorkDirectory 'inventory'
$env:FETCHPATH_APP_DATA_DIR = Join-Path $WorkDirectory 'data'
foreach ($name in $environmentNames) { [IO.Directory]::CreateDirectory([Environment]::GetEnvironmentVariable($name)) | Out-Null }
$codexConfig = Join-Path $env:CODEX_HOME 'config.toml'
$claudeConfig = Join-Path $env:CLAUDE_CONFIG_DIR '.claude.json'
[IO.File]::WriteAllText($codexConfig, "# FP-102 unrelated settings sentinel`nsuppress_unstable_features_warning = true`n")
[IO.File]::WriteAllText($claudeConfig, '{"fp102Sentinel":"preserve-me","mcpServers":{}}')
$observation = [ordered]@{ task = 'FP-102'; mode = $(if ($RunHosts) { 'real-host' } else { 'lifecycle-only' }); passed = $false; hosts = @(); lifecycle = @(); failures = @(); limitations = @('No installer/component removal or clean-machine uninstall is exercised by this script.') }
$processes = [Collections.Generic.List[Diagnostics.Process]]::new()
$authCopies = [Collections.Generic.List[string]]::new()
$fixture = $null

function Assert-True([bool] $Value, [string] $Message) { if (-not $Value) { throw $Message } }
function Invoke-Bounded([string] $Executable, [string[]] $Arguments, [int] $Seconds = 30) {
    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.WorkingDirectory = $WorkDirectory
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start)
    $processes.Add($process)
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $timedOut = -not $process.WaitForExit($Seconds * 1000)
    if ($timedOut) {
        $process.Kill($true)
        $process.WaitForExit()
    }
    $output = $stdout.GetAwaiter().GetResult()
    $errors = $stderr.GetAwaiter().GetResult()
    # Failed output may contain private sign-in URLs. Retain it only in ignored
    # scratch and export a fixed category, never a matched line or error message.
    if ($timedOut -or $process.ExitCode -ne 0) {
        $baseName = 'failure-' + $processes.Count + '-' + [IO.Path]::GetFileNameWithoutExtension($Executable)
        $stdoutName = "$baseName.stdout.txt"
        $stderrName = "$baseName.stderr.txt"
        [IO.File]::WriteAllText((Join-Path $WorkDirectory $stdoutName), $output)
        [IO.File]::WriteAllText((Join-Path $WorkDirectory $stderrName), $errors)
        $failureText = "$output`n$errors"
        $category = if ($timedOut) { 'timeout' }
            elseif ($failureText -match '(?i)unexpected argument|unrecognized (argument|option)|unknown option') { 'unsupported-cli-option' }
            elseif ($failureText -match '(?i)model.{0,100}(not found|not supported|unsupported|does not exist|not available|access)|unsupported.{0,40}model') { 'model-unavailable' }
            elseif ($failureText -match '(?i)sandbox.{0,100}(initializ|setup|failed|not supported)|windows sandbox') { 'sandbox-initialization' }
            elseif ($failureText -match '(?i)not logged in|login required|sign.in|authentication|unauthorized|invalid.{0,20}(token|api key)|401') { 'authentication' }
            else { 'unclassified' }
        $observation.failures += [ordered]@{ executable = [IO.Path]::GetFileName($Executable); exitCode = $process.ExitCode; category = $category; stdout = $stdoutName; stderr = $stderrName }
        throw "$(Split-Path -Leaf $Executable) failed ($category); diagnostics retained in isolated scratch as $baseName.*.txt."
    }
    return [pscustomobject]@{ stdout = $output; stderr = $errors }
}
function Invoke-Fetchpath([string[]] $Arguments, [string] $Executable = $ApplicationPath) {
    return (Invoke-Bounded $Executable $Arguments).stdout
}
function Assert-Sentinels {
    Assert-True ([IO.File]::ReadAllText($codexConfig).Contains('suppress_unstable_features_warning = true')) 'Codex unrelated setting was changed.'
    Assert-True (([IO.File]::ReadAllText($claudeConfig) | ConvertFrom-Json).fp102Sentinel -eq 'preserve-me') 'Claude unrelated setting was changed.'
}
function Assert-HostToolResults([string] $HostName, [object[]] $Calls, [object[]] $Results) {
    $failed = @(if ($HostName -eq 'codex') {
        @($Calls | Where-Object {
            $_.item.status -ne 'completed' -or
            $null -eq $_.item.result -or
            ($_.item.PSObject.Properties['error'] -and $null -ne $_.item.error) -or
            ($_.item.result -and $_.item.result.PSObject.Properties['isError'] -and $_.item.result.isError)
        })
    } else {
        @($Results | Where-Object { $_.PSObject.Properties['is_error'] -and $_.is_error })
    })
    if ($failed.Count -gt 0 -or $Results.Count -ne 2) {
        $text = @($Calls, $Results) | ConvertTo-Json -Depth 30
        $category = if ($text -match '(?i)requires approval|approval policy|permission.{0,40}(denied|required)|not allowed') { 'host-tool-approval' } else { 'host-tool-failure' }
        $observation.failures += [ordered]@{ host = $HostName; category = $category; events = "$HostName-events.jsonl" }
        throw "$HostName tool calls failed ($category); inspect isolated host events locally."
    }
}
function Wait-Until([scriptblock] $Predicate, [int] $Seconds, [string] $Description) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (& $Predicate) { return }
        Start-Sleep -Milliseconds 200
    }
    throw "Timed out waiting for $Description."
}

try {
    if ($CopyLocalAuth) {
        foreach ($pair in @(@((Join-Path $realCodexHome 'auth.json'), (Join-Path $env:CODEX_HOME 'auth.json')), @((Join-Path $realClaudeHome '.credentials.json'), (Join-Path $env:CLAUDE_CONFIG_DIR '.credentials.json')))) {
            if (Test-Path -LiteralPath $pair[0] -PathType Leaf) {
                Copy-Item -LiteralPath $pair[0] -Destination $pair[1]
                $authCopies.Add($pair[1])
            }
        }
    }
    foreach ($hostName in $Hosts) {
        Invoke-Fetchpath @('agent-setup', 'add', $hostName) | Out-Null
        $configPath = if ($hostName -eq 'codex') { $codexConfig } else { $claudeConfig }
        $first = [IO.File]::ReadAllText($configPath)
        Invoke-Fetchpath @('agent-setup', 'add', $hostName) | Out-Null
        Assert-True ([IO.File]::ReadAllText($configPath) -eq $first) "$hostName repeated add changed configuration."
        Assert-Sentinels
        $observation.lifecycle += "$hostName explicit add and idempotent repeat"
    }
    Invoke-Fetchpath @('agent-setup', 'status', '--json') | ConvertFrom-Json | Out-Null
    $policies = (Invoke-Fetchpath @('agents', '--json') | ConvertFrom-Json).policies
    Assert-True (@($policies).Count -eq 0) 'Registration implicitly granted agent access.'
    $movedDirectory = Join-Path $WorkDirectory 'custom install with spaces'
    [IO.Directory]::CreateDirectory($movedDirectory) | Out-Null
    $movedExe = Join-Path $movedDirectory 'fetchpath.exe'
    Copy-Item -LiteralPath $ApplicationPath -Destination $movedExe
    Invoke-Fetchpath @('agent-setup', 'reconcile') $movedExe | Out-Null
    foreach ($hostName in $Hosts) {
        $path = if ($hostName -eq 'codex') { $codexConfig } else { $claudeConfig }
        $text = [IO.File]::ReadAllText($path).Replace('\\', '\')
        Assert-True ($text.Contains($movedExe) -or $text.Contains($movedExe.Replace('\', '/'))) "$hostName reconcile did not update to the path with spaces."
    }
    Assert-Sentinels
    $observation.lifecycle += 'reconcile after executable path change with spaces'
    foreach ($hostName in $Hosts) {
        Invoke-Fetchpath @('agent-setup', 'check', $hostName) $movedExe | Out-Null
        $observation.lifecycle += "$hostName owned registration MCP handshake and tool discovery"
    }

    if ($RunHosts) {
        # Fixture is an independent child; no web URL or publisher claim is involved.
        $fixtureScript = Join-Path $WorkDirectory 'fixture.mjs'
        [IO.File]::WriteAllText($fixtureScript, @'
import http from 'node:http';
const body = Buffer.alloc(65536, 0x5a);
http.createServer((req, res) => {
  res.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': body.length });
  res.end(body);
}).listen(0, '127.0.0.1', function () { console.log(this.address().port); });
'@)
        $start = [Diagnostics.ProcessStartInfo]::new((Get-Command node -CommandType Application | Select-Object -First 1).Source)
        $start.ArgumentList.Add($fixtureScript)
        $start.UseShellExecute = $false
        $start.CreateNoWindow = $true
        $start.RedirectStandardOutput = $true
        $fixture = [Diagnostics.Process]::Start($start)
        $processes.Add($fixture)
        $portTask = $fixture.StandardOutput.ReadLineAsync()
        Assert-True ($portTask.Wait(10000)) 'Fixture did not report its port.'
        $port = [int] $portTask.Result
        foreach ($hostName in $Hosts) {
            $hostExe = (Get-Command $(if ($hostName -eq 'codex') { 'codex' } else { 'claude' }) -CommandType Application | Select-Object -First 1).Source
            $version = (Invoke-Bounded $hostExe @('--version')).stdout.Trim()
            $granted = Join-Path $WorkDirectory "$hostName-granted"
            $ungranted = Join-Path $WorkDirectory "$hostName-ungranted"
            [IO.Directory]::CreateDirectory($granted) | Out-Null
            [IO.Directory]::CreateDirectory($ungranted) | Out-Null
            Invoke-Fetchpath @('agents', 'grant', $hostName, $granted) $movedExe | Out-Null
            $firstArguments = @{ url = "http://127.0.0.1:$port/$hostName-granted.bin"; folder = $granted; kind = 'file'; wait = $true; timeout_seconds = 30 } | ConvertTo-Json -Compress
            $secondArguments = @{ url = "http://127.0.0.1:$port/$hostName-ungranted.bin"; folder = $ungranted; kind = 'file'; wait = $false } | ConvertTo-Json -Compress
            $prompt = "Use only the registered Fetchpath MCP tools. Never use shell, filesystem, or HTTP tools. Call download exactly twice with these exact JSON arguments, first: $firstArguments then: $secondArguments . Do not approve, cancel, or change either job. Report the two tool results and stop."
            $arguments = if ($hostName -eq 'codex') {
                # Explicit permission for this fixture invocation only. Fetchpath
                # folder grants and its human-approval boundary still apply.
                @('exec', '--json', '--ephemeral', '--skip-git-repo-check', '--sandbox', 'read-only', '--model', $CodexModel, '-c', 'model_reasoning_effort="low"', '-c', 'mcp_servers.fetchpath.tools.download.approval_mode="approve"', '-c', ('mcp_servers.fetchpath.env.FETCHPATH_APP_DATA_DIR=' + ($env:FETCHPATH_APP_DATA_DIR | ConvertTo-Json -Compress)), $prompt)
            } else {
                @('--print', '--output-format', 'stream-json', '--verbose', '--no-session-persistence', '--model', 'sonnet', '--effort', 'low', '--tools', '', '--allowedTools', 'mcp__fetchpath__*', '--', $prompt)
            }
            $result = Invoke-Bounded $hostExe $arguments $HostTimeoutSeconds
            [IO.File]::WriteAllText((Join-Path $WorkDirectory "$hostName-events.jsonl"), $result.stdout)
            $toolEvents = @($result.stdout -split '\r?\n' | Where-Object { $_.Trim().StartsWith('{') } | ForEach-Object { $_ | ConvertFrom-Json })
            $calls = if ($hostName -eq 'codex') {
                @($toolEvents | Where-Object { $_.type -eq 'item.completed' -and $_.PSObject.Properties['item'] -and $_.item.type -eq 'mcp_tool_call' -and $_.item.server -eq 'fetchpath' -and $_.item.tool -eq 'download' })
            } else {
                @($toolEvents | Where-Object { $_.type -eq 'assistant' } | ForEach-Object { $_.message.content } | Where-Object { $_.type -eq 'tool_use' -and $_.name -eq 'mcp__fetchpath__download' })
            }
            Assert-True ($calls.Count -eq 2) "$hostName did not record exactly two actual Fetchpath download tool calls."
            $toolResults = if ($hostName -eq 'codex') { $calls } else {
                @($toolEvents | Where-Object { $_.type -eq 'user' -and $_.PSObject.Properties['message'] } | ForEach-Object { $_.message.content } | Where-Object { $_.type -eq 'tool_result' })
            }
            Assert-HostToolResults $hostName $calls @($toolResults)
            Assert-True (($toolResults | ConvertTo-Json -Depth 30).Contains('awaiting_approval')) "$hostName tool results did not report awaiting_approval."
            $saved = Join-Path $granted "$hostName-granted.bin"
            Wait-Until { Test-Path -LiteralPath $saved -PathType Leaf } 30 "$hostName completed bytes"
            $bytes = [IO.File]::ReadAllBytes($saved)
            Assert-True ($bytes.Length -eq 65536 -and @($bytes | Where-Object { $_ -ne 0x5a }).Count -eq 0) "$hostName saved incorrect fixture bytes."
            Assert-True (-not (Test-Path -LiteralPath (Join-Path $ungranted "$hostName-ungranted.bin"))) "$hostName wrote into an ungranted folder."
            $jobs = (Invoke-Fetchpath @('ls', '--json') $movedExe | ConvertFrom-Json).jobs
            $waiting = @($jobs | Where-Object { $_.PSObject.Properties['destination'] -and $_.destination -like "*$hostName-ungranted.bin" -and $_.state -eq 'awaiting_approval' -and $_.principal -eq "agent:$hostName" })
            Assert-True ($waiting.Count -eq 1) "$hostName ungranted job was not awaiting human approval in the engine."
            $completed = @($jobs | Where-Object { $_.PSObject.Properties['destination'] -and $_.destination -eq $saved -and $_.state -eq 'completed' -and $_.principal -eq "agent:$hostName" })
            Assert-True ($completed.Count -eq 1) "$hostName granted job did not complete as the registered agent principal."
            $observation.hosts += [ordered]@{ host = $hostName; version = $version; scope = $(if ($hostName -eq 'codex') { 'CODEX_HOME/config.toml' } else { 'CLAUDE_CONFIG_DIR/.claude.json user' }); model = $(if ($hostName -eq 'codex') { $CodexModel } else { 'sonnet' }); toolCalls = $calls.Count; savedBytes = $bytes.Length; ungrantedState = 'awaiting_approval' }
        }
    } else { $observation.limitations += 'Real host discovery, authentication, model calls and downloads were not exercised.' }
    foreach ($hostName in $Hosts) { Invoke-Fetchpath @('agent-setup', 'remove', $hostName) $movedExe | Out-Null }
    Invoke-Fetchpath @('agent-setup', 'cleanup') $movedExe | Out-Null
    Assert-Sentinels
    foreach ($path in @($codexConfig, $claudeConfig)) {
        $text = [IO.File]::ReadAllText($path).Replace('\\', '\').Replace('/', '\')
        Assert-True (-not $text.Contains($movedExe)) 'Owned registration survived removal.'
    }
    $observation.lifecycle += 'selected-host remove and repeated cleanup preserve unrelated settings'
    $observation.passed = $true
} finally {
    # Stop the isolated engine before restoring data/home overrides.
    try { Invoke-Fetchpath @('engine', 'stop') | Out-Null } catch { $observation.limitations += 'Isolated engine stop failed; inspect scratch before reusing it.' }
    foreach ($process in $processes) { if (-not $process.HasExited) { $process.Kill($true); $process.WaitForExit() }; $process.Dispose() }
    foreach ($path in $authCopies) { Remove-Item -LiteralPath $path -Force }
    foreach ($name in $environmentNames) { [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name]) }
    [IO.File]::WriteAllText([IO.Path]::GetFullPath($OutputPath), ($observation | ConvertTo-Json -Depth 8))
}
Write-Output "FP-102 evidence: $OutputPath"
