[CmdletBinding()]
param(
    [string] $ApplicationPath
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
if (-not $ApplicationPath) {
    $ApplicationPath = Join-Path $repositoryRoot 'target\release\fetchpath-desktop.exe'
}
$ApplicationPath = [System.IO.Path]::GetFullPath($ApplicationPath)
if (-not (Test-Path -LiteralPath $ApplicationPath -PathType Leaf)) {
    throw "Desktop executable not found: $ApplicationPath"
}

$outputDirectory = [System.IO.Path]::GetFullPath((Join-Path $repositoryRoot 'work\desktop-e2e'))
if (-not $outputDirectory.StartsWith($repositoryRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw 'Refusing to use an output directory outside the repository.'
}
[System.IO.Directory]::CreateDirectory($outputDirectory) | Out-Null

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class FetchpathWindowChildren
{
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")]
    public static extern bool EnumChildWindows(IntPtr hWnd, EnumWindowsProc callback, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Auto)]
    public static extern int GetClassName(IntPtr hWnd, StringBuilder className, int maxCount);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern void keybd_event(byte virtualKey, byte scanCode, uint flags, UIntPtr extraInfo);

    public static void PressAndRelease(byte virtualKey)
    {
        keybd_event(virtualKey, 0, 0, UIntPtr.Zero);
        keybd_event(virtualKey, 0, 2, UIntPtr.Zero);
    }

    public static List<IntPtr> Get(IntPtr parent)
    {
        var handles = new List<IntPtr>();
        EnumChildWindows(parent, (handle, _) => { handles.Add(handle); return true; }, IntPtr.Zero);
        return handles;
    }

    public static string ClassName(IntPtr handle)
    {
        var name = new StringBuilder(256);
        GetClassName(handle, name, name.Capacity);
        return name.ToString();
    }
}
'@

function Get-FreePort {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    try { return ([System.Net.IPEndPoint] $listener.LocalEndpoint).Port }
    finally { $listener.Stop() }
}

function Start-FixtureServer([int] $Size, [int] $DelayMilliseconds) {
    $port = Get-FreePort
    $job = Start-Job -ArgumentList $port, $Size, $DelayMilliseconds -ScriptBlock {
        param($Port, $Size, $DelayMilliseconds)
        $ErrorActionPreference = 'Stop'
        $listener = [System.Net.HttpListener]::new()
        $listener.Prefixes.Add("http://127.0.0.1:$Port/")
        $listener.Start()
        try {
            Write-Output 'READY'
            $context = $listener.GetContext()
            $response = $context.Response
            $response.StatusCode = 200
            $response.ContentType = 'application/octet-stream'
            $response.ContentLength64 = $Size
            $chunk = [byte[]]::new(16384)
            [System.Array]::Fill($chunk, [byte] 0x5A)
            $remaining = $Size
            try {
                while ($remaining -gt 0) {
                    $count = [Math]::Min($chunk.Length, $remaining)
                    $response.OutputStream.Write($chunk, 0, $count)
                    $response.OutputStream.Flush()
                    $remaining -= $count
                    if ($DelayMilliseconds -gt 0) { Start-Sleep -Milliseconds $DelayMilliseconds }
                }
            } catch [System.Net.HttpListenerException] {
                # Cancellation closes the client connection before this fixture completes.
            } finally {
                $response.OutputStream.Close()
            }
        } finally {
            $listener.Stop()
            $listener.Close()
        }
    }

    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        $ready = @(Receive-Job -Job $job -Keep) -contains 'READY'
        if ($ready) { break }
        if ($job.State -eq 'Failed') {
            Receive-Job -Job $job -Wait
            throw 'Fixture server failed to start.'
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $ready) { throw 'Fixture server did not become ready.' }

    [pscustomobject]@{
        Job = $job
        Url = "http://127.0.0.1:$port/download.bin"
    }
}

function Find-ById($Root, [string] $AutomationId) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty,
        $AutomationId
    )
    return $Root.FindFirst([System.Windows.Automation.TreeScope]::Subtree, $condition)
}

function Get-RendererRoot($Process) {
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    do {
        $Process.Refresh()
        if ($Process.MainWindowHandle -ne 0) {
            foreach ($handle in [FetchpathWindowChildren]::Get($Process.MainWindowHandle)) {
                if ([FetchpathWindowChildren]::ClassName($handle) -ne 'Chrome_RenderWidgetHostHWND') { continue }
                $candidate = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
                if (Find-ById $candidate 'url') { return $candidate }
            }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'The desktop renderer did not expose its accessible controls.'
}

function Set-Field($Root, [string] $AutomationId, [string] $Value) {
    $element = Find-ById $Root $AutomationId
    if (-not $element) { throw "Field not found: $AutomationId" }
    $pattern = $element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    $pattern.SetValue($Value)
}

function Invoke-Control($Root, [string] $AutomationId, $Process) {
    $invoked = $false
    for ($attempt = 0; $attempt -lt 8 -and -not $invoked; $attempt++) {
        $element = Find-ById $Root $AutomationId
        if (-not $element) {
            Start-Sleep -Milliseconds 100
            continue
        }
        try {
            [FetchpathWindowChildren]::SetForegroundWindow($Process.MainWindowHandle) | Out-Null
            $element.SetFocus()
            $pattern = $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
            $pattern.Invoke()
            $invoked = $true
        } catch {
            if ($_.Exception.InnerException -is [System.Windows.Automation.ElementNotAvailableException] -or
                $_.Exception -is [System.Windows.Automation.ElementNotAvailableException]) {
                Start-Sleep -Milliseconds 100
                continue
            }
            throw
        }
    }
    if (-not $invoked) { throw "Control could not be invoked: $AutomationId" }
    if ($AutomationId -eq 'start-download') {
        Start-Sleep -Milliseconds 500
        if (-not (Find-ById $Root 'job-card')) {
            $element.SetFocus()
            [FetchpathWindowChildren]::PressAndRelease(0x0D)
        }
    }
}

function Get-ControlText($Root, [string] $AutomationId) {
    $element = Find-ById $Root $AutomationId
    if (-not $element) { return $null }
    if ($element.Current.Name) { return $element.Current.Name }
    $children = $element.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )
    $names = for ($index = 0; $index -lt $children.Count; $index++) {
        $name = $children.Item($index).Current.Name
        if ($name) { $name }
    }
    return $names -join ' '
}

function Find-VisibleText($Root, [string] $Pattern) {
    $items = $Root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )
    for ($index = 0; $index -lt $items.Count; $index++) {
        $name = $items.Item($index).Current.Name
        if ($name -match $Pattern) { return $name }
    }
    return $null
}

function Wait-ForStatus($Root, [string[]] $Statuses, [int] $TimeoutSeconds = 30) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $status = Get-ControlText $Root 'job-status'
        if ($Statuses -contains $status) { return $status }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    $visible = $Root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )
    $summary = for ($index = 0; $index -lt $visible.Count; $index++) {
        $item = $visible.Item($index)
        if ($item.Current.AutomationId -or $item.Current.Name) {
            "id=$($item.Current.AutomationId);name=$($item.Current.Name)"
        }
    }
    throw "Download did not reach status: $($Statuses -join ', '). Last status: $status. Visible controls: $($summary -join ' | ')"
}

$process = $null
$fixtureJobs = [System.Collections.Generic.List[object]]::new()
$completedPath = Join-Path $outputDirectory 'completed.bin'
$cancelledPath = Join-Path $outputDirectory 'cancelled.bin'

foreach ($path in @($completedPath, $cancelledPath)) {
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
}

try {
    $process = Start-Process -FilePath $ApplicationPath -PassThru
    $renderer = Get-RendererRoot $process

    Set-Field $renderer 'url' "https://example.test/one.bin`nhttps://example.test/two.bin"
    Set-Field $renderer 'destination' (Join-Path $outputDirectory 'batch-one.bin')
    $urlControl = Find-ById $renderer 'url'
    [FetchpathWindowChildren]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
    $urlControl.SetFocus()
    [FetchpathWindowChildren]::PressAndRelease(0x58)
    [FetchpathWindowChildren]::PressAndRelease(0x08)
    Start-Sleep -Milliseconds 200
    $previewDeadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        $batchButtonLabel = Get-ControlText $renderer 'start-download'
        if ($batchButtonLabel -eq 'Add 2 to queue') { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $previewDeadline)
    if ($batchButtonLabel -ne 'Add 2 to queue') { throw "Batch preview did not expose two items. Submit label: $batchButtonLabel" }
    $previewCount = '2 items'

    Set-Field $renderer 'url' ''
    Set-Field $renderer 'destination' ''

    $completeFixture = Start-FixtureServer -Size (1024 * 1024) -DelayMilliseconds 0
    $fixtureJobs.Add($completeFixture.Job)
    Set-Field $renderer 'url' $completeFixture.Url
    Set-Field $renderer 'destination' $completedPath
    Invoke-Control $renderer 'start-download' $process
    $completeStatus = Wait-ForStatus $renderer @('Complete', 'Needs attention')
    if ($completeStatus -ne 'Complete') {
        throw 'Real download failed in the desktop UI.'
    }
    if (-not (Test-Path -LiteralPath $completedPath -PathType Leaf)) {
        throw 'Completed download was not published to its destination.'
    }
    $completedBytes = (Get-Item -LiteralPath $completedPath).Length
    if ($completedBytes -ne 1024 * 1024) { throw "Unexpected completed byte count: $completedBytes" }
    $diskHash = (Get-FileHash -LiteralPath $completedPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $displayedHash = (Find-VisibleText $renderer '^[a-f0-9]{64}$').ToLowerInvariant()
    if ($diskHash -ne $displayedHash) { throw 'Displayed observed hash does not match the downloaded file.' }

    $cancelFixture = Start-FixtureServer -Size (32 * 1024 * 1024) -DelayMilliseconds 15
    $fixtureJobs.Add($cancelFixture.Job)
    Set-Field $renderer 'url' $cancelFixture.Url
    Set-Field $renderer 'destination' $cancelledPath
    Invoke-Control $renderer 'start-download' $process
    $startedStatus = Wait-ForStatus $renderer @('Queued', 'Downloading', 'Needs attention') 15
    if ($startedStatus -eq 'Needs attention') {
        throw "Slow download failed before progress: $(Get-ControlText $renderer 'job-error')"
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        $received = Get-ControlText $renderer 'job-bytes'
        if ($received -and $received -ne '0 B received') { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $received -or $received -eq '0 B received') {
        throw "Slow download never exposed progress. Status: $(Get-ControlText $renderer 'job-status'); form error: $(Get-ControlText $renderer 'form-error')"
    }
    Invoke-Control $renderer 'cancel-download' $process
    $cancelStatus = Wait-ForStatus $renderer @('Cancelled', 'Complete', 'Needs attention')
    if ($cancelStatus -ne 'Cancelled') { throw "Expected cancellation, reached: $cancelStatus" }
    if (Test-Path -LiteralPath $cancelledPath) { throw 'Cancelled download published a destination file.' }

    [pscustomobject]@{
        application = $ApplicationPath
        batchPreviewCount = $previewCount
        completedStatus = $completeStatus
        completedBytes = $completedBytes
        observedSha256 = $displayedHash
        progressObserved = $received
        cancelledStatus = $cancelStatus
        cancelledDestinationPublished = $false
    } | ConvertTo-Json -Depth 3
} finally {
    if ($process -and -not $process.HasExited) { Stop-Process -Id $process.Id -Force }
    foreach ($job in $fixtureJobs) {
        if ($job.State -eq 'Running') { Stop-Job -Job $job }
        Remove-Job -Job $job -Force
    }
}
