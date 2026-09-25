# Fetchpath desktop smoke walkthrough (FP-012, FP-031; repaired for FP-055).
#
# Drives the optimized desktop executable through Windows UI Automation with
# localhost fixtures, against its own engine in a scratch data folder under
# work\desktop-e2e, so a person's queue is never touched. It checks:
#
#   1. a two-link batch preview;
#   2. a real 1 MiB download, published, with the displayed SHA-256 equal to
#      the file on disk;
#   3. progress on a throttled download, then cancellation publishing nothing;
#   4. a wrong pasted checksum saving nothing and offering Edit checksum, and a
#      matching one (pasted as `SHA256:` in capitals) completing.
#
# Input goes to the renderer window by window messages (uia-common.ps1), not
# global keystrokes, so it does not type into whatever else has focus.

[CmdletBinding()]
param(
    [string] $ApplicationPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
. (Join-Path $repositoryRoot 'tests\compatibility\windows\uia-common.ps1')

if (-not $ApplicationPath) {
    $ApplicationPath = Join-Path $repositoryRoot 'target\release\fetchpath-desktop.exe'
}
$ApplicationPath = [System.IO.Path]::GetFullPath($ApplicationPath)
if (-not (Test-Path -LiteralPath $ApplicationPath -PathType Leaf)) {
    throw "Desktop executable not found: $ApplicationPath"
}
# The desktop starts the engine from beside itself.
$engine = Join-Path (Split-Path $ApplicationPath) 'fetchpath.exe'
if (-not (Test-Path -LiteralPath $engine -PathType Leaf)) {
    throw "The engine is not beside the desktop app: $engine (cargo build --release -p fetchpath)"
}

$outputDirectory = [System.IO.Path]::GetFullPath((Join-Path $repositoryRoot 'work\desktop-e2e'))
if (-not $outputDirectory.StartsWith($repositoryRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Refusing to use an output directory outside the repository.'
}
$dataDirectory = Join-Path $outputDirectory 'data'
if (Test-Path -LiteralPath $outputDirectory) { Remove-Item -LiteralPath $outputDirectory -Recurse -Force }
[System.IO.Directory]::CreateDirectory($outputDirectory) | Out-Null
$env:FETCHPATH_APP_DATA_DIR = $dataDirectory

function Find-VisibleText($Root, [string] $Pattern) {
    $items = Get-DescendantElements $Root
    for ($index = 0; $index -lt $items.Count; $index++) {
        $name = $items.Item($index).Current.Name
        if ($name -and $name -match $Pattern) { return $name }
    }
    return $null
}

function Open-AdvancedOptions($Root) {
    if (Find-ById $Root 'checksum') { return }
    if (-not (Find-ById $Root 'advanced-summary')) { Open-Popup $Root 'add-open' }
    # The dialog keeps Advanced options open from the last time.
    if (Find-ById $Root 'checksum') { return }
    $summary = Find-ById $Root 'advanced-summary'
    if (-not $summary) { throw 'Advanced options are not exposed.' }
    $pattern = $null
    if ($summary.TryGetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern, [ref] $pattern)) {
        $pattern.Expand()
    } else {
        $summary.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
    }
    Wait-ForCondition { Find-ById $Root 'checksum' } 5 'the checksum field under Advanced options' | Out-Null
}

# Adds one download through the Add download dialog.
function Add-Download($Root, $Process, [string] $Url, [string] $Destination, [string] $Checksum = $null) {
    if ($Checksum) { Open-AdvancedOptions $Root }
    Set-Field $Root 'url' $Url
    Set-Field $Root 'destination' $Destination
    if ($Checksum) { Set-Field $Root 'checksum' $Checksum }
    Invoke-Control $Root 'start-download' $Process
    Wait-ForCondition { -not (Find-ById $Root 'start-download') } 10 'the Add download dialog to close' | Out-Null
    # The newest job is the first card; wait for it, so a status read next is
    # this job's and not the previous one's.
    $line = '^' + [regex]::Escape($Destination) + '$'
    Wait-ForCondition { Find-VisibleText $Root $line } 15 "the card for $Destination" | Out-Null
}

$process = $null
$fixtures = [System.Collections.Generic.List[object]]::new()
$completedPath = Join-Path $outputDirectory 'completed.bin'
$cancelledPath = Join-Path $outputDirectory 'cancelled.bin'
$checksumMismatchPath = Join-Path $outputDirectory 'checksum-mismatch.bin'
$checksumMatchPath = Join-Path $outputDirectory 'checksum-match.bin'

try {
    $process = Start-Process -FilePath $ApplicationPath -PassThru
    $renderer = Get-RendererRoot $process
    $renderWidget = Get-RenderWidgetHandle $process

    # 1. The preview reacts to typing, so a character is typed and removed.
    Set-Field $renderer 'url' "https://example.test/one.bin`nhttps://example.test/two.bin"
    Set-Field $renderer 'destination' (Join-Path $outputDirectory 'batch-one.bin')
    (Find-ById $renderer 'url').SetFocus()
    [FetchpathUia]::PostText($renderWidget, 'x')
    [FetchpathUia]::PostKey($renderWidget, [byte] 0x08)  # Backspace
    $batchButtonLabel = $null
    $previewDeadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        $batchButtonLabel = Get-ControlText $renderer 'start-download'
        if ($batchButtonLabel -eq 'Add 2 to queue') { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $previewDeadline)
    if ($batchButtonLabel -ne 'Add 2 to queue') { throw "Batch preview did not expose two items. Submit label: $batchButtonLabel" }
    Invoke-Control $renderer 'add-cancel' $process
    Wait-ForCondition { -not (Find-ById $renderer 'start-download') } 10 'the Add download dialog to close' | Out-Null

    # 2. A real download.
    $completeFixture = Start-FixtureServer -Size (1024 * 1024)
    $fixtures.Add($completeFixture)
    Add-Download $renderer $process "$($completeFixture.BaseUrl)/completed.bin" $completedPath
    $completeStatus = Wait-ForStatus $renderer @('Complete', 'Needs attention')
    if ($completeStatus -ne 'Complete') { throw 'Real download failed in the desktop UI.' }
    if (-not (Test-Path -LiteralPath $completedPath -PathType Leaf)) { throw 'Completed download was not published to its destination.' }
    $completedBytes = (Get-Item -LiteralPath $completedPath).Length
    if ($completedBytes -ne 1024 * 1024) { throw "Unexpected completed byte count: $completedBytes" }
    $diskHash = (Get-FileHash -LiteralPath $completedPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $displayedHash = Wait-ForCondition { Find-VisibleText $renderer '^[a-f0-9]{64}$' } 10 'the observed SHA-256'
    if ($diskHash -ne $displayedHash.ToLowerInvariant()) { throw 'Displayed observed hash does not match the downloaded file.' }

    # 3. Progress, then cancellation.
    $cancelFixture = Start-FixtureServer -Size (32 * 1024 * 1024) -DelayMilliseconds 15
    $fixtures.Add($cancelFixture)
    Add-Download $renderer $process "$($cancelFixture.BaseUrl)/cancelled.bin" $cancelledPath
    $startedStatus = Wait-ForStatus $renderer @('Queued', 'Downloading', 'Needs attention') 15
    if ($startedStatus -eq 'Needs attention') { throw 'Slow download failed before progress.' }
    $received = $null
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        $received = Get-ControlText $renderer 'job-bytes'
        if ($received -and $received -ne '0 B received') { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $received -or $received -eq '0 B received') { throw "Slow download never exposed progress. Status: $(Get-ControlText $renderer 'job-status')" }
    Invoke-Control $renderer 'cancel-download' $process
    $cancelStatus = Wait-ForStatus $renderer @('Cancelled', 'Complete', 'Needs attention')
    if ($cancelStatus -ne 'Cancelled') { throw "Expected cancellation, reached: $cancelStatus" }
    if (Test-Path -LiteralPath $cancelledPath) { throw 'Cancelled download published a destination file.' }

    # 4. A pasted checksum: a mismatch saves nothing and says so; a match completes.
    $checksumSize = 256 * 1024
    $fixtureBytes = [byte[]]::new($checksumSize)
    for ($offset = 0; $offset -lt $checksumSize; $offset++) { $fixtureBytes[$offset] = [byte] 0x5A }
    $expectedHash = -join ([System.Security.Cryptography.SHA256]::Create().ComputeHash($fixtureBytes) | ForEach-Object { $_.ToString('x2') })
    $checksumFixture = Start-FixtureServer -Size $checksumSize
    $fixtures.Add($checksumFixture)

    Add-Download $renderer $process "$($checksumFixture.BaseUrl)/mismatch.bin" $checksumMismatchPath ('0' * 64)
    $mismatchStatus = Wait-ForStatus $renderer @('Complete', 'Needs attention')
    if ($mismatchStatus -ne 'Needs attention') { throw "A wrong checksum reached: $mismatchStatus" }
    if (Test-Path -LiteralPath $checksumMismatchPath) { throw 'A checksum mismatch published a file.' }
    # A paragraph reaches UI Automation as text without its id, so the
    # message is found by what it says.
    $mismatchMessage = Find-VisibleText $renderer "doesn't match the checksum"
    if (-not $mismatchMessage) { throw 'The mismatch row does not say the file did not match the checksum.' }
    if (-not (Find-VisibleText $renderer '^Edit checksum: ')) { throw 'The mismatch row does not offer Edit checksum.' }
    if (-not (Find-VisibleText $renderer "^$expectedHash$")) { throw 'The mismatch row does not show the received SHA-256.' }

    # Pasted the way checksum listings often print it.
    Add-Download $renderer $process "$($checksumFixture.BaseUrl)/match.bin" $checksumMatchPath ('SHA256:' + $expectedHash.ToUpperInvariant())
    $matchStatus = Wait-ForStatus $renderer @('Complete', 'Needs attention')
    if ($matchStatus -ne 'Complete') { throw "A matching checksum reached: $matchStatus" }
    $matchedHash = (Get-FileHash -LiteralPath $checksumMatchPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($matchedHash -ne $expectedHash) { throw 'The published file does not match the checksum it was accepted against.' }
    $matchLabel = Wait-ForCondition { Find-VisibleText $renderer 'matches the checksum you entered' } 10 'the checksum match label'

    [pscustomobject]@{
        application = $ApplicationPath
        batchPreviewLabel = $batchButtonLabel
        completedStatus = $completeStatus
        completedBytes = $completedBytes
        observedSha256 = $diskHash
        progressObserved = $received
        cancelledStatus = $cancelStatus
        cancelledDestinationPublished = $false
        checksumMismatchStatus = $mismatchStatus
        checksumMismatchPublished = $false
        checksumMismatchMessage = $mismatchMessage
        checksumMatchStatus = $matchStatus
        checksumMatchLabel = $matchLabel
        checksumExpected = $expectedHash
    } | ConvertTo-Json -Depth 3
} finally {
    if ($process -and -not $process.HasExited) { Stop-Process -Id $process.Id -Force }
    # The engine outlives the window by design; this run's one is stopped.
    & $engine engine stop | Out-Null
    foreach ($fixture in $fixtures) { Stop-FixtureServer $fixture }
}
