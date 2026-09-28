# Fetchpath desktop: the content cache (FP-032, A01, A02).
#
# Drives the real optimized executable through Windows UI Automation, in its
# own data folder (FETCHPATH_APP_DATA_DIR), so the person's queue and cache
# are never touched:
#
#   1. a checksum download through the engine fills the cache;
#   2. with the server gone, the same file added in Add download with the
#      same checksum completes from the cache, byte-identical, and its row
#      says so instead of showing a speed;
#   3. Settings shows the cache's use and bounds, every control in the Cache
#      section has a name, and Clear cache empties it in the engine.
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
$cliPath = Join-Path (Split-Path $ApplicationPath) 'fetchpath.exe'
foreach ($path in @($ApplicationPath, $cliPath)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Not found: $path" }
}
$workDirectory = Join-Path $repositoryRoot 'work\fp032-cache'
if (-not $OutputPath) { $OutputPath = Join-Path $workDirectory 'ui-cache.json' }
if (Test-Path -LiteralPath $workDirectory) { Remove-Item -LiteralPath $workDirectory -Recurse -Force }
$dataDirectory = Join-Path $workDirectory 'data'
$downloads = Join-Path $workDirectory 'downloads'
foreach ($folder in @($dataDirectory, $downloads)) {
    [System.IO.Directory]::CreateDirectory($folder) | Out-Null
}
$env:FETCHPATH_APP_DATA_DIR = $dataDirectory
Remove-Item Env:FETCHPATH_DATA_DIR -ErrorAction SilentlyContinue

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
# The fixture serves $size bytes of 0x5A.
$body = [byte[]]::new($size)
for ($offset = 0; $offset -lt $size; $offset++) { $body[$offset] = [byte] 0x5A }
$sha256 = ([System.BitConverter]::ToString([System.Security.Cryptography.SHA256]::Create().ComputeHash($body)) -replace '-', '').ToLowerInvariant()

function Find-VisibleText($Root, [string] $Pattern) {
    $items = Get-DescendantElements $Root
    for ($index = 0; $index -lt $items.Count; $index++) {
        $info = Get-ElementInfo $items.Item($index)
        if ($info -and $info.name -and $info.name -match $Pattern) { return $info.name }
    }
    return $null
}

function Get-Cache {
    $json = & $cliPath cache --json
    if ($LASTEXITCODE -ne 0) { throw "fetchpath cache failed: $json" }
    return ($json | ConvertFrom-Json).cache
}

try {
    & $cliPath settings onboarding-completed on | Out-Null

    # 1. Fill the cache through the engine.
    $fixture = Start-FixtureServer -Size $size
    $first = & $cliPath add "$($fixture.BaseUrl)/first.bin" --to $downloads --sha256 $sha256 --wait
    Assert-True ($LASTEXITCODE -eq 0) "The first download failed: $first"
    # The copy into the cache follows the completion.
    $cache = Wait-ForCondition { $c = Get-Cache; if ($c.entries -gt 0) { $c } } 10 'the cache to fill'
    $observation.cacheAfterFirst = $cache
    Assert-True ($cache.entries -eq 1 -and $cache.bytes -eq $size) "The cache holds $($cache.entries) entries, $($cache.bytes) bytes."
    Stop-FixtureServer $fixture
    $fixture = $null

    # 2. The same file from a link nothing serves, added in the desktop.
    $process = Start-Process -FilePath $ApplicationPath -PassThru
    $renderer = Get-RendererRoot $process
    $renderWidget = Get-RenderWidgetHandle $process
    Wait-ForCondition { Find-ById $renderer 'add-open' } 30 'the window to load' | Out-Null
    $null = Set-ControlFocus $renderer 'add-open' $process
    Start-Sleep -Milliseconds 300
    [FetchpathUia]::PostKey($renderWidget, $VK.Enter)
    Wait-ForCondition { Find-ById $renderer 'url' } 10 'Add download to open' | Out-Null
    $again = Join-Path $downloads 'again.bin'
    Set-Field $renderer 'url' "http://127.0.0.1:$(Get-ClosedPort)/again.bin"
    Start-Sleep -Milliseconds 600
    Set-Field $renderer 'destination' $again
    $null = Set-ControlFocus $renderer 'advanced-summary' $process
    Start-Sleep -Milliseconds 300
    [FetchpathUia]::PostKey($renderWidget, $VK.Enter)
    Wait-ForCondition { Find-ById $renderer 'checksum' } 10 'the checksum field' | Out-Null
    Set-Field $renderer 'checksum' $sha256
    Start-Sleep -Milliseconds 300
    Invoke-Control $renderer 'start-download' $process
    Wait-ForCondition { (Test-Path -LiteralPath $again) -and (Get-Item -LiteralPath $again).Length -eq $size } 40 'the reused file to be saved' | Out-Null
    $observation.reusedSha256 = (Get-FileHash -LiteralPath $again -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-True ($observation.reusedSha256 -eq $sha256) 'The reused file differs from the original.'
    $observation.rowLabel = Wait-ForCondition { Find-VisibleText $renderer "^Reused from this computer's cache$" } 20 'the row to say it was reused'
    $observation.rowShowsSpeed = [bool] (Find-VisibleText $renderer '/s$')
    Assert-True (-not $observation.rowShowsSpeed) 'A reused row shows a speed.'

    # 3. Settings shows the cache and clears it.
    Open-Popup $renderer 'open-settings'
    Wait-ForCondition { Find-ById $renderer 'cache-group' } 20 'the Cache section' | Out-Null
    $observation.usage = Wait-ForCondition {
        $text = Get-ControlText $renderer 'cache-usage'
        if ($text -match 'in 1 file') { $text }
    } 10 'the cache usage'
    $observation.quotaField = Get-FieldValue $renderer 'setting-cache-quota'
    Assert-True ($observation.quotaField -eq '2') "The quota field reads $($observation.quotaField)."

    $unnamed = @()
    $items = Get-DescendantElements (Find-ById $renderer 'cache-group')
    for ($index = 0; $index -lt $items.Count; $index++) {
        $info = Get-ElementInfo $items.Item($index)
        if (-not $info -or -not $info.isKeyboardFocusable) { continue }
        if ($interactiveTypes -notcontains $info.controlType) { continue }
        if (-not $info.name) { $unnamed += "$($info.controlType) $($info.automationId)" }
    }
    $observation.unnamedCacheControls = $unnamed
    Assert-True ($unnamed.Count -eq 0) "Unnamed controls in Cache: $($unnamed -join '; ')"

    Invoke-Control $renderer 'cache-clear' $process
    Wait-ForCondition { (Get-Cache).entries -eq 0 } 10 'the cache to be cleared' | Out-Null
    $observation.usageAfterClear = Wait-ForCondition {
        $text = Get-ControlText $renderer 'cache-usage'
        if ($text -match '^Empty') { $text }
    } 10 'the usage to say empty'
    Assert-True ((Test-Path -LiteralPath $again) -and (Test-Path -LiteralPath (Join-Path $downloads 'first.bin'))) 'Clearing the cache removed a saved file.'
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
if ($failures.Count) { throw "ui-cache failed:`n$($failures -join "`n")" }
Write-Output "ui-cache passed. Observation: $OutputPath"
