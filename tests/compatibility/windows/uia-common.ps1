# Shared Windows UI Automation helpers for the Fetchpath desktop compatibility
# scripts. Dot-source this file; it defines functions only and runs no checks.
#
# The pattern follows apps/desktop/tools/ui-smoke.ps1: a Tauri window hosts the
# WebView2 renderer in a child window of class `Chrome_RenderWidgetHostHWND`, and
# the accessible controls live under that child, not under the top-level window.

Set-StrictMode -Version Latest

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

if (-not ('FetchpathUia' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class FetchpathUia
{
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")]
    public static extern bool EnumChildWindows(IntPtr hWnd, EnumWindowsProc callback, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Auto)]
    public static extern int GetClassName(IntPtr hWnd, StringBuilder className, int maxCount);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern bool BringWindowToTop(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern void SwitchToThisWindow(IntPtr hWnd, bool altTab);

    private const uint WM_KEYDOWN = 0x0100;
    private const uint WM_KEYUP = 0x0101;
    private const uint WM_CHAR = 0x0102;

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr SendMessageW(IntPtr hWnd, uint message, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern bool PostMessageW(IntPtr hWnd, uint message, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr SendMessageTimeoutW(
        IntPtr hWnd, uint message, IntPtr wParam, IntPtr lParam,
        uint flags, uint timeoutMilliseconds, out IntPtr result);

    private const uint SMTO_ABORTIFHUNG = 0x0002;

    // Keyboard input delivered straight to the renderer window. Chromium handles
    // posted WM_KEYDOWN/WM_KEYUP for focus traversal and WM_CHAR for text, which
    // works even when another process is holding the foreground and synthetic
    // global input (keybd_event) would be swallowed.
    //
    // A posted key goes through the application's message pump, so
    // TranslateMessage also synthesises the matching WM_CHAR. That is what makes
    // Enter activate a button - Blink activates on the keypress event - but it
    // also means a posted VK_TAB delivers a literal 0x09 character, and the
    // character arrives *after* focus has already moved, so it lands in the text
    // field the Tab moved into. Use SendKey for keys whose character must not be
    // typed, and PostKey for keys that need it.
    public static void PostKey(IntPtr hWnd, byte virtualKey)
    {
        PostMessageW(hWnd, WM_KEYDOWN, (IntPtr)virtualKey, IntPtr.Zero);
        Thread.Sleep(10);
        PostMessageW(hWnd, WM_KEYUP, (IntPtr)virtualKey, IntPtr.Zero);
    }

    // The same key press, delivered straight to the window procedure instead of
    // through the message queue. TranslateMessage never sees it, so no WM_CHAR is
    // synthesised: focus traversal (VK_TAB) and the editing commands Blink runs
    // on keydown (VK_ESCAPE, VK_BACK) still happen, with no character typed
    // anywhere. Keys that Blink acts on at keypress time - Enter activating a
    // button - are not delivered by this route and must use PostKey.
    //
    // SendMessage across processes blocks until the target pumps messages, so the
    // call is bounded and abandons a hung window rather than hanging the run.
    public static bool SendKey(IntPtr hWnd, byte virtualKey)
    {
        IntPtr result;
        bool delivered = SendMessageTimeoutW(
            hWnd, WM_KEYDOWN, (IntPtr)virtualKey, IntPtr.Zero,
            SMTO_ABORTIFHUNG, 4000, out result) != IntPtr.Zero;
        Thread.Sleep(10);
        SendMessageTimeoutW(
            hWnd, WM_KEYUP, (IntPtr)virtualKey, IntPtr.Zero,
            SMTO_ABORTIFHUNG, 4000, out result);
        return delivered;
    }

    private const uint WM_CLOSE = 0x0010;

    // The same request the window's close button sends, delivered without
    // needing the window to be in the foreground.
    public static void PostClose(IntPtr hWnd)
    {
        PostMessageW(hWnd, WM_CLOSE, IntPtr.Zero, IntPtr.Zero);
    }

    public static void PostText(IntPtr hWnd, string text)
    {
        foreach (char character in text)
        {
            PostMessageW(hWnd, WM_CHAR, (IntPtr)character, IntPtr.Zero);
            Thread.Sleep(4);
        }
    }

    // Synthetic keystrokes go to whatever window is in the foreground, and
    // Windows refuses foreground changes requested by a background process
    // unless coaxed. Without this, Tab presses silently go nowhere and a focus
    // order check passes or fails for the wrong reason.
    public static bool EnsureForeground(IntPtr hWnd)
    {
        for (int attempt = 0; attempt < 25; attempt++)
        {
            if (GetForegroundWindow() == hWnd) { return true; }
            BringWindowToTop(hWnd);
            SetForegroundWindow(hWnd);
            if (GetForegroundWindow() != hWnd) { SwitchToThisWindow(hWnd, true); }
            Thread.Sleep(80);
        }
        return GetForegroundWindow() == hWnd;
    }

    [DllImport("user32.dll")]
    public static extern void keybd_event(byte virtualKey, byte scanCode, uint flags, UIntPtr extraInfo);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern short VkKeyScanW(char character);

    [DllImport("shcore.dll")]
    private static extern int GetProcessDpiAwareness(IntPtr process, out int awareness);

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hWnd);

    // 0 = unaware, 1 = system DPI aware, 2 = per-monitor DPI aware.
    public static int DpiAwareness(IntPtr process)
    {
        int awareness;
        if (GetProcessDpiAwareness(process, out awareness) != 0) { return -1; }
        return awareness;
    }

    public static uint WindowDpi(IntPtr hWnd)
    {
        return GetDpiForWindow(hWnd);
    }

    private const uint KEYEVENTF_KEYUP = 2;
    private const byte VK_SHIFT = 0x10;
    private const byte VK_CONTROL = 0x11;
    private const byte VK_MENU = 0x12;

    public static void PressAndRelease(byte virtualKey)
    {
        keybd_event(virtualKey, 0, 0, UIntPtr.Zero);
        keybd_event(virtualKey, 0, KEYEVENTF_KEYUP, UIntPtr.Zero);
    }

    public static void PressWithControl(byte virtualKey)
    {
        keybd_event(VK_CONTROL, 0, 0, UIntPtr.Zero);
        PressAndRelease(virtualKey);
        keybd_event(VK_CONTROL, 0, KEYEVENTF_KEYUP, UIntPtr.Zero);
    }

    public static void PressWithShift(byte virtualKey)
    {
        keybd_event(VK_SHIFT, 0, 0, UIntPtr.Zero);
        PressAndRelease(virtualKey);
        keybd_event(VK_SHIFT, 0, KEYEVENTF_KEYUP, UIntPtr.Zero);
    }

    // Real synthetic keystrokes for the active keyboard layout, so a text field
    // is filled the way a person fills it rather than through a value pattern.
    public static bool SendText(string text)
    {
        foreach (char character in text)
        {
            short scan = VkKeyScanW(character);
            if (scan == -1) { return false; }
            byte virtualKey = (byte)(scan & 0xFF);
            int modifiers = (scan >> 8) & 0xFF;
            if ((modifiers & 1) != 0) { keybd_event(VK_SHIFT, 0, 0, UIntPtr.Zero); }
            if ((modifiers & 2) != 0) { keybd_event(VK_CONTROL, 0, 0, UIntPtr.Zero); }
            if ((modifiers & 4) != 0) { keybd_event(VK_MENU, 0, 0, UIntPtr.Zero); }
            PressAndRelease(virtualKey);
            if ((modifiers & 4) != 0) { keybd_event(VK_MENU, 0, KEYEVENTF_KEYUP, UIntPtr.Zero); }
            if ((modifiers & 2) != 0) { keybd_event(VK_CONTROL, 0, KEYEVENTF_KEYUP, UIntPtr.Zero); }
            if ((modifiers & 1) != 0) { keybd_event(VK_SHIFT, 0, KEYEVENTF_KEYUP, UIntPtr.Zero); }
            Thread.Sleep(6);
        }
        return true;
    }

    public static List<IntPtr> Children(IntPtr parent)
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
}

$script:VK = @{
    Tab = [byte] 0x09
    Enter = [byte] 0x0D
    Escape = [byte] 0x1B
    Space = [byte] 0x20
    End = [byte] 0x23
    Delete = [byte] 0x2E
    A = [byte] 0x41
    L = [byte] 0x4C
    F4 = [byte] 0x73
}

function Get-VirtualKeys { return $script:VK }

function Get-FreePort {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    try { return ([System.Net.IPEndPoint] $listener.LocalEndpoint).Port }
    finally { $listener.Stop() }
}

# A loopback port with nothing listening, used to produce a real, deterministic
# transfer failure without reaching the network.
function Get-ClosedPort {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = ([System.Net.IPEndPoint] $listener.LocalEndpoint).Port
    $listener.Stop()
    return $port
}

function Start-FixtureServer([int] $Size, [int] $DelayMilliseconds = 0) {
    $port = Get-FreePort
    $job = Start-Job -ArgumentList $port, $Size, $DelayMilliseconds -ScriptBlock {
        param($Port, $Size, $DelayMilliseconds)
        $ErrorActionPreference = 'Stop'
        $listener = [System.Net.HttpListener]::new()
        $listener.Prefixes.Add("http://127.0.0.1:$Port/")
        $listener.Start()
        try {
            Write-Output 'READY'
            while ($true) {
                $context = $listener.GetContext()
                $response = $context.Response
                $response.StatusCode = 200
                $response.ContentType = 'application/octet-stream'
                $response.ContentLength64 = $Size
                $chunk = [byte[]]::new(16384)
                # Filled with a loop rather than [System.Array]::Fill, which
                # does not exist on .NET Framework and so throws under Windows
                # PowerShell 5.1. It threw here on the first request, which
                # stopped the listener and made the application's transfer fail
                # to connect - a dead fixture masquerading as an app defect.
                for ($offset = 0; $offset -lt $chunk.Length; $offset++) { $chunk[$offset] = [byte] 0x5A }
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
                } finally {
                    $response.OutputStream.Close()
                }
            }
        } finally {
            $listener.Stop()
            $listener.Close()
        }
    }

    $ready = $false
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
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

    [pscustomobject]@{ Job = $job; Port = $port; BaseUrl = "http://127.0.0.1:$port" }
}

# The health of the fixture server itself. A listener that died mid-run makes
# the application's transfer fail for a reason that has nothing to do with the
# application, so a check that depends on a download records this alongside the
# result rather than blaming the app.
function Get-FixtureDiagnostics($Fixture) {
    if (-not $Fixture) { return [ordered]@{ state = 'absent' } }
    $messages = @(Receive-Job -Job $Fixture.Job -Keep 2>&1 | ForEach-Object { [string] $_ } | Where-Object { $_ -ne 'READY' })
    return [ordered]@{
        state = [string] $Fixture.Job.State
        port = $Fixture.Port
        unexpectedOutput = $messages
        errors = @($Fixture.Job.ChildJobs | ForEach-Object { $_.Error } | ForEach-Object { [string] $_ })
    }
}

function Stop-FixtureServer($Fixture) {
    if (-not $Fixture) { return }
    if ($Fixture.Job.State -eq 'Running') { Stop-Job -Job $Fixture.Job }
    Remove-Job -Job $Fixture.Job -Force
}

# Every synthetic keystroke goes through here so the window is confirmed to be
# in the foreground first; otherwise the key is delivered to another application.
function Send-Key([byte] $VirtualKey, $Process, [string] $Modifier = '') {
    if (-not [FetchpathUia]::EnsureForeground($Process.MainWindowHandle)) {
        throw 'The Fetchpath window could not be brought to the foreground, so keystrokes cannot be delivered.'
    }
    switch ($Modifier) {
        'Control' { [FetchpathUia]::PressWithControl($VirtualKey) }
        'Shift' { [FetchpathUia]::PressWithShift($VirtualKey) }
        default { [FetchpathUia]::PressAndRelease($VirtualKey) }
    }
    Start-Sleep -Milliseconds 140
}

function Send-KeyText([string] $Text, $Process) {
    if (-not [FetchpathUia]::EnsureForeground($Process.MainWindowHandle)) {
        throw 'The Fetchpath window could not be brought to the foreground, so text cannot be typed.'
    }
    return [FetchpathUia]::SendText($Text)
}

function Find-ById($Root, [string] $AutomationId) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty,
        $AutomationId
    )
    return $Root.FindFirst([System.Windows.Automation.TreeScope]::Subtree, $condition)
}

# The comma keeps the AutomationElementCollection intact: returning it bare lets
# PowerShell unroll it, which turns an empty result into $null and a single
# result into a plain element, and then `.Count` fails under StrictMode.
function Get-DescendantElements($Root) {
    return , $Root.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )
}

# A snapshot of one element's properties, or $null when it cannot be read.
# Walking a live Chromium accessibility tree turns up elements that go away or
# report a default-initialised property set between the search and the read, and
# one of those must not abort a whole run.
function Get-ElementInfo($Element) {
    if (-not $Element) { return $null }
    try {
        $current = $Element.Current
        $type = $current.ControlType
        if (-not $type) { return $null }
        return [pscustomobject]@{
            element = $Element
            automationId = $current.AutomationId
            name = $current.Name
            controlType = $type.ProgrammaticName -replace '^ControlType\.', ''
            isKeyboardFocusable = $current.IsKeyboardFocusable
            isOffscreen = $current.IsOffscreen
        }
    } catch {
        return $null
    }
}

# The element inside the renderer that holds DOM keyboard focus. This is read
# per element rather than from AutomationElement::FocusedElement, because that
# reports the system-wide focus, which belongs to another process whenever the
# Fetchpath window is not the foreground window.
function Get-RendererFocus($Root) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::HasKeyboardFocusProperty, $true
    )
    $element = $Root.FindFirst([System.Windows.Automation.TreeScope]::Subtree, $condition)
    $info = Get-ElementInfo $element
    if (-not $info) { return $null }
    return [pscustomobject]@{
        automationId = $info.automationId
        name = $info.name
        controlType = $info.controlType
    }
}

function Get-RenderWidgetHandle($Process) {
    foreach ($handle in [FetchpathUia]::Children($Process.MainWindowHandle)) {
        if ([FetchpathUia]::ClassName($handle) -eq 'Chrome_RenderWidgetHostHWND') { return $handle }
    }
    throw 'The renderer window was not found, so keyboard input cannot be delivered.'
}

function Get-RendererRoot($Process, [int] $TimeoutSeconds = 40) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $Process.Refresh()
        if ($Process.MainWindowHandle -ne 0) {
            foreach ($handle in [FetchpathUia]::Children($Process.MainWindowHandle)) {
                if ([FetchpathUia]::ClassName($handle) -ne 'Chrome_RenderWidgetHostHWND') { continue }
                $candidate = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
                if (Find-ById $candidate 'add-open') { return $candidate }
            }
        }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'The desktop renderer did not expose its accessible controls.'
}

function Open-Popup($Root, [string] $AutomationId) {
    # A button with aria-haspopup is exposed by Chromium through
    # ExpandCollapse rather than Invoke; both perform its default action.
    $element = Find-ById $Root $AutomationId
    if (-not $element) { throw "Control not found: $AutomationId" }
    $invoke = $null
    if ($element.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref] $invoke)) {
        $invoke.Invoke()
    } else {
        $element.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    }
    Start-Sleep -Milliseconds 600
}

function Set-Field($Root, [string] $AutomationId, [string] $Value) {
    $element = Find-ById $Root $AutomationId
    # FP-035: the composer lives in the Add download dialog, which is out of
    # the accessibility tree until it opens. Open it the way a person would.
    if (-not $element -and (Find-ById $Root 'add-open')) {
        Open-Popup $Root 'add-open'
        $element = Find-ById $Root $AutomationId
    }
    if (-not $element) { throw "Field not found: $AutomationId" }
    $element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value)
}

function Get-FieldValue($Root, [string] $AutomationId) {
    $element = Find-ById $Root $AutomationId
    if (-not $element) { return $null }
    return $element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
}

function Invoke-Control($Root, [string] $AutomationId, $Process) {
    for ($attempt = 0; $attempt -lt 10; $attempt++) {
        $element = Find-ById $Root $AutomationId
        if (-not $element) { Start-Sleep -Milliseconds 120; continue }
        try {
            [FetchpathUia]::SetForegroundWindow($Process.MainWindowHandle) | Out-Null
            $element.SetFocus()
            $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
            return
        } catch {
            Start-Sleep -Milliseconds 120
        }
    }
    throw "Control could not be invoked: $AutomationId"
}

function Set-ControlFocus($Root, [string] $AutomationId, $Process) {
    $element = Find-ById $Root $AutomationId
    if (-not $element) { throw "Control not found: $AutomationId" }
    [FetchpathUia]::SetForegroundWindow($Process.MainWindowHandle) | Out-Null
    $element.SetFocus()
    Start-Sleep -Milliseconds 120
    return $element
}

function Get-ControlText($Root, [string] $AutomationId) {
    $element = Find-ById $Root $AutomationId
    if (-not $element) { return $null }
    if ($element.Current.Name) { return $element.Current.Name }
    $children = Get-DescendantElements $element
    $names = for ($index = 0; $index -lt $children.Count; $index++) {
        $name = $children.Item($index).Current.Name
        if ($name) { $name }
    }
    return ($names -join ' ')
}

function Get-FocusedDescriptor {
    $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
    if (-not $focused) { return $null }
    try {
        return [pscustomobject]@{
            automationId = $focused.Current.AutomationId
            name = $focused.Current.Name
            controlType = $focused.Current.ControlType.ProgrammaticName -replace '^ControlType\.', ''
            isKeyboardFocusable = $focused.Current.IsKeyboardFocusable
        }
    } catch {
        return $null
    }
}

function Get-AriaProperties($Element) {
    try {
        $value = $Element.GetCurrentPropertyValue([System.Windows.Automation.AutomationElement]::AriaPropertiesProperty)
        if ($null -eq $value) { return '' }
        return [string] $value
    } catch {
        return ''
    }
}

function Get-AriaRole($Element) {
    try {
        $value = $Element.GetCurrentPropertyValue([System.Windows.Automation.AutomationElement]::AriaRoleProperty)
        if ($null -eq $value) { return '' }
        return [string] $value
    } catch {
        return ''
    }
}

function Get-HelpText($Element) {
    try {
        $value = $Element.Current.HelpText
        if ($null -eq $value) { return '' }
        return [string] $value
    } catch {
        return ''
    }
}

function Wait-ForCondition([scriptblock] $Condition, [int] $TimeoutSeconds = 20, [string] $Description = 'condition') {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $result = & $Condition
        if ($result) { return $result }
        Start-Sleep -Milliseconds 120
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for $Description."
}

function Wait-ForStatus($Root, [string[]] $Statuses, [int] $TimeoutSeconds = 40) {
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $status = $null
    do {
        $status = Get-ControlText $Root 'job-status'
        if ($Statuses -contains $status) { return $status }
        Start-Sleep -Milliseconds 120
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Download did not reach status: $($Statuses -join ', '). Last status: $status"
}

function Get-RepositoryRoot([string] $ScriptDirectory) {
    return [System.IO.Path]::GetFullPath((Join-Path $ScriptDirectory '..\..\..'))
}
