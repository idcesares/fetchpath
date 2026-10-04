# FP-102: runs only inside a disposable Windows Sandbox. Host command stubs
# exercise installer discovery/registration; actual host calls are agent-hosts.ps1.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$InstallerPath, [Parameter(Mandatory)][string]$OutputPath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$checks = [System.Collections.Generic.List[string]]::new()
$result = [ordered]@{ environment = 'Windows Sandbox'; hostDiscovery = 'cmd fixtures (not actual agent calls)'; passed = $false; checks = @(); error = $null }
function Assert-Check([bool]$Condition, [string]$Name) {
    if (-not $Condition) { throw "Failed: $Name" }
    $checks.Add($Name)
}
function Run-Installer([string[]]$Arguments) {
    $p = Start-Process -FilePath $InstallerPath -ArgumentList $Arguments -PassThru -WindowStyle Hidden
    if (-not $p.WaitForExit(120000)) { $p.Kill(); throw 'Installer timed out' }
    if ($p.ExitCode -ne 0) { throw "Installer exit $($p.ExitCode)" }
}
function Uninstall([bool]$DeleteData, [switch]$Replacement) {
    $args = @('/S')
    if ($DeleteData) { $args += '/DELETEAPPDATA' }
    if ($Replacement) { $args += '/REPLACE' }
    $p = Start-Process -FilePath (Join-Path $install 'uninstall.exe') -ArgumentList $args -PassThru -WindowStyle Hidden
    if (-not $p.WaitForExit(120000)) { $p.Kill(); throw 'Uninstall timed out' }
    if ($p.ExitCode -ne 0) { throw "Uninstall exit $($p.ExitCode)" }
    for ($i=0; $i -lt 30 -and (Test-Path (Join-Path $install 'fetchpath.exe')); $i++) { Start-Sleep -Seconds 1 }
}
function Read-Codex { return [System.IO.File]::ReadAllText((Join-Path $env:CODEX_HOME 'config.toml')) }
function Read-Claude { return (Get-Content -LiteralPath (Join-Path $env:CLAUDE_CONFIG_DIR '.claude.json') -Raw | ConvertFrom-Json) }
function Assert-Connected {
    Assert-Check ((Read-Codex) -match '\[mcp_servers\.fetchpath\]') 'Codex registered'
    Assert-Check ($null -ne (Read-Claude).mcpServers.PSObject.Properties['fetchpath']) 'Claude Code registered'
    Assert-Check ((Read-Codex) -match 'unrelated') 'Codex unrelated configuration preserved'
    Assert-Check ((Read-Claude).sentinel -eq 'keep') 'Claude unrelated configuration preserved'
}
function Assert-Removed {
    Assert-Check ((Read-Codex) -notmatch '\[mcp_servers\.fetchpath\]') 'Codex owned registration removed'
    Assert-Check ($null -eq (Read-Claude).mcpServers.PSObject.Properties['fetchpath']) 'Claude owned registration removed'
    Assert-Check ((Read-Codex) -match 'unrelated') 'Codex sentinel survives removal'
    Assert-Check ((Read-Claude).sentinel -eq 'keep') 'Claude sentinel survives removal'
}
function Run-GuidedSetup {
    if (-not ('AgentSetupNative' -as [type])) { Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public sealed class AgentSetupControl {
    public IntPtr Handle;
    public string ClassName;
    public string Text;
    public int Id;
    public bool Enabled;
}
public sealed class AgentSetupDialog {
    public IntPtr Handle;
    public string ClassName;
    public string Text;
    public AgentSetupControl[] Controls;
}
public static class AgentSetupNative {
    public delegate bool Callback(IntPtr window, IntPtr parameter);
    [DllImport("user32.dll")] static extern bool EnumWindows(Callback callback, IntPtr parameter);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr window, Callback callback, IntPtr parameter);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr window, StringBuilder text, int count);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr window, StringBuilder text, int count);
    [DllImport("user32.dll")] static extern bool IsWindowEnabled(IntPtr window);
    [DllImport("user32.dll")] static extern int GetDlgCtrlID(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr GetParent(IntPtr window);
    [DllImport("user32.dll", SetLastError = true)] static extern bool PostMessage(IntPtr window, uint message, IntPtr wparam, IntPtr lparam);
    [DllImport("user32.dll", SetLastError = true)] static extern IntPtr SendMessageTimeout(IntPtr window, uint message, IntPtr wparam, IntPtr lparam, uint flags, uint timeout, out UIntPtr result);
    static string Class(IntPtr window) { var text = new StringBuilder(128); GetClassName(window, text, text.Capacity); return text.ToString(); }
    static string Text(IntPtr window) { var text = new StringBuilder(512); GetWindowText(window, text, text.Capacity); return text.ToString(); }
    public static AgentSetupDialog[] Find(int process) {
        var result = new List<AgentSetupDialog>();
        EnumWindows((window, parameter) => { uint owner; GetWindowThreadProcessId(window, out owner);
            if (owner == process) {
                var controls = new List<AgentSetupControl>();
                EnumChildWindows(window, (child, unused) => {
                    controls.Add(new AgentSetupControl { Handle = child, ClassName = Class(child), Text = Text(child), Id = GetDlgCtrlID(child), Enabled = IsWindowEnabled(child) });
                    return true;
                }, IntPtr.Zero);
                result.Add(new AgentSetupDialog { Handle = window, ClassName = Class(window), Text = Text(window), Controls = controls.ToArray() });
            }
            return true;
        }, IntPtr.Zero);
        return result.ToArray();
    }
    static UIntPtr Send(IntPtr window, uint message, IntPtr wparam) {
        UIntPtr result;
        if (SendMessageTimeout(window, message, wparam, IntPtr.Zero, 2, 1000, out result) == IntPtr.Zero) throw new InvalidOperationException("Native checkbox message timed out.");
        return result;
    }
    public static int Check(IntPtr window) { return (int)Send(window, 0x00F0, IntPtr.Zero).ToUInt32(); }
    public static void SetChecked(IntPtr window) { Send(window, 0x00F1, new IntPtr(1)); }
    public static void Advance(IntPtr button) {
        IntPtr parent = GetParent(button);
        if (parent == IntPtr.Zero || !PostMessage(parent, 0x0111, new IntPtr(GetDlgCtrlID(button) & 0xffff), button)) throw new InvalidOperationException("Cannot post installer navigation command.");
    }
}
'@
    }
    $p = Start-Process -FilePath $InstallerPath -PassThru -WindowStyle Hidden
    $deadline = [DateTime]::UtcNow.AddSeconds(120)
    $discoveryDeadline = [DateTime]::UtcNow.AddSeconds(15)
    $selected = $false
    $foundNativeDialog = $false
    $lastAdvancedFingerprint = $null
    try {
        while (-not $p.HasExited -and [DateTime]::UtcNow -lt $deadline) {
            # Hidden launch deliberately means visibility is not a selector.
            # NSIS can own helper windows before the actual #32770 dialog.
            $windows = @([AgentSetupNative]::Find($p.Id))
            $result.guidedNativeWindows = @($windows | Select-Object -First 6 | ForEach-Object {
                [ordered]@{ class = $_.ClassName; title = $_.Text; controls = @($_.Controls | Select-Object -First 40 | ForEach-Object { [ordered]@{ class = $_.ClassName; text = $_.Text; id = $_.Id; enabled = $_.Enabled } }) }
            })
            $dialogs = @($windows | Where-Object { $_.ClassName -eq '#32770' -and @($_.Controls | Where-Object { $_.ClassName -eq 'Button' }).Count -gt 0 })
            if ($dialogs.Count -eq 0) {
                if (-not $foundNativeDialog -and [DateTime]::UtcNow -ge $discoveryDeadline) { throw 'No native installer dialog with Button children found after 15 seconds; see bounded native window diagnostics.' }
                Start-Sleep -Milliseconds 300
                continue
            }
            $foundNativeDialog = $true
            # Prefer the dialog owning navigation, rather than a nested/helper dialog.
            $window = @($dialogs | Where-Object { @($_.Controls | Where-Object { $_.ClassName -eq 'Button' -and $_.Text.Replace('&','').Trim() -match '^(Next\s*>?|I Agree|Install|Finish)$' }).Count -gt 0 } | Select-Object -First 1)
            if ($window.Count -eq 0) { Start-Sleep -Milliseconds 300; continue }
            $controls = @($window[0].Controls)
            $fingerprint = ($controls | ForEach-Object { "$($_.Handle):$($_.ClassName):$($_.Id):$($_.Enabled):$($_.Text)" }) -join '|'
            if (-not $selected) {
                $codex = @($controls | Where-Object { $_.ClassName -eq 'Button' -and $_.Text -eq 'Connect Codex (user configuration)' })
                $claude = @($controls | Where-Object { $_.ClassName -eq 'Button' -and $_.Text -eq 'Connect Claude Code (user configuration)' })
                if ($codex.Count -eq 1 -and $claude.Count -eq 1) {
                    foreach ($box in @($codex[0],$claude[0])) {
                        Assert-Check $box.Enabled 'Detected host checkbox enabled'
                        Assert-Check ([AgentSetupNative]::Check($box.Handle) -eq 0) 'Host connection starts unchecked'
                        [AgentSetupNative]::SetChecked($box.Handle)
                        Assert-Check ([AgentSetupNative]::Check($box.Handle) -eq 1) 'Explicit host checkbox selection read back checked'
                    }
                    $selected = $true
                } elseif ($codex.Count -gt 0 -or $claude.Count -gt 0) {
                    throw 'Host-selection page has ambiguous or missing native checkboxes.'
                }
            }
            $button = @($controls | Where-Object { $_.Enabled -and $_.ClassName -eq 'Button' -and $_.Text.Replace('&','').Trim() -match '^(Next\s*>?|I Agree|Install|Finish)$' } | Select-Object -First 1)
            if ($button.Count -eq 1 -and $fingerprint -ne $lastAdvancedFingerprint) {
                [AgentSetupNative]::Advance($button[0].Handle)
                $lastAdvancedFingerprint = $fingerprint
            }
            Start-Sleep -Milliseconds 500
        }
        if (-not $p.HasExited) { $p.Kill(); throw 'Guided setup UI timed out' }
        Assert-Check $selected 'Interactive host-selection page exercised'
        Assert-Check ($p.ExitCode -eq 0) 'Interactive setup completed'
        $result.Remove('guidedNativeWindows')
    } finally { if (-not $p.HasExited) { $p.Kill() }; $p.Dispose() }
}
try {
    # Existing packaging harness convention: unsigned local builds are blocked
    # by SAC in this disposable image. Never change the owner's host policy.
    $sacKey = 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy'
    $sac = Get-ItemProperty -LiteralPath $sacKey -Name VerifiedAndReputablePolicyState -ErrorAction SilentlyContinue
    $result.smartAppControlState = if ($sac) { [int]$sac.VerifiedAndReputablePolicyState } else { 0 }
    if ($result.smartAppControlState -ne 0) {
        Set-ItemProperty -LiteralPath $sacKey -Name VerifiedAndReputablePolicyState -Value 0 -Type DWord
        $refresh = Start-Process -FilePath 'CiTool.exe' -ArgumentList '--refresh','--json' -PassThru -WindowStyle Hidden
        if (-not $refresh.WaitForExit(60000)) { $refresh.Kill(); throw 'Sandbox SAC refresh timed out' }
    }
    $scratch = Join-Path $env:TEMP 'fp102-agent-installer'
    New-Item -ItemType Directory -Path $scratch -Force | Out-Null
    $localInstaller = Join-Path $scratch 'setup.exe'
    Copy-Item -LiteralPath $InstallerPath -Destination $localInstaller
    $InstallerPath = $localInstaller
    $bin = Join-Path $scratch 'hosts'
    New-Item -ItemType Directory -Path $bin -Force | Out-Null
    foreach ($hostName in @('codex','claude')) { [System.IO.File]::WriteAllText((Join-Path $bin "$hostName.cmd"), "@echo off`r`nexit /b 0`r`n") }
    $env:PATH = "$bin;$env:PATH"
    $env:CODEX_HOME = Join-Path $scratch 'codex'
    $env:CLAUDE_CONFIG_DIR = Join-Path $scratch 'claude'
    foreach ($dir in @($env:CODEX_HOME,$env:CLAUDE_CONFIG_DIR)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    [System.IO.File]::WriteAllText((Join-Path $env:CODEX_HOME 'config.toml'), "# unrelated sentinel`n[mcp_servers.unrelated]`ncommand = 'unrelated'`n")
    [System.IO.File]::WriteAllText((Join-Path $env:CLAUDE_CONFIG_DIR '.claude.json'), '{"sentinel":"keep","mcpServers":{"unrelated":{"command":"unrelated"}}}')
    $install = Join-Path $env:LOCALAPPDATA 'Fetchpath'
    foreach ($delete in @($false,$true)) {
        Run-Installer @('/S','/COMPONENTS=cli,mcp')
        Assert-Removed
        if (-not $delete) { Run-GuidedSetup }
        else { Run-Installer @('/S','/COMPONENTS=cli,mcp','/AGENTHOSTS=codex,claude-code') }
        Assert-Connected
        Run-Installer @('/S')
        Assert-Connected
        Run-Installer @('/S','/UPDATE')
        Assert-Connected
        # The internal marker used by uninstall-first version replacement.
        Uninstall $false -Replacement
        Assert-Connected
        Run-Installer @('/S','/COMPONENTS=cli,mcp')
        Assert-Connected
        $checks.Add('Uninstall-first replacement retains and reconciles connections')
        Run-Installer @('/S','/COMPONENTS=cli')
        Assert-Removed
        Run-Installer @('/S','/COMPONENTS=cli,mcp','/AGENTHOSTS=codex,claude-code')
        Assert-Connected
        $data = Join-Path $env:APPDATA 'app.fetchpath.desktop'
        New-Item -ItemType Directory -Path $data -Force | Out-Null
        [System.IO.File]::WriteAllText((Join-Path $data 'fp102-retention-sentinel'), 'keep unless explicitly deleting app data')
        $download = Join-Path $scratch 'completed-download.bin'
        [System.IO.File]::WriteAllText($download, 'user download')
        Uninstall $delete
        Assert-Removed
        Assert-Check (-not (Test-Path (Join-Path $install 'fetchpath.exe'))) 'Program removed'
        # NSIS relocates its uninstaller into TEMP; the original process may
        # return before the relocated process finishes POSTUNINSTALL cleanup.
        if ($delete) {
            for ($i=0; $i -lt 45 -and (Test-Path (Join-Path $data 'fp102-retention-sentinel')); $i++) { Start-Sleep -Seconds 1 }
        }
        Assert-Check ((Test-Path (Join-Path $data 'fp102-retention-sentinel')) -eq (-not $delete)) "Application data retention choice $delete"
        Assert-Check ((Get-Content -LiteralPath $download -Raw) -eq 'user download') 'Completed download survives'
    }
    $result.passed = $true
} catch { $result.error = $_.Exception.Message }
finally {
    $result.assertions = $checks.Count
    $result.checks = @($checks.ToArray() | Select-Object -Unique)
    $result.osBuild = [string][System.Environment]::OSVersion.Version
    [System.IO.File]::WriteAllText($OutputPath, ($result | ConvertTo-Json -Depth 8))
    [System.IO.File]::WriteAllText((Join-Path (Split-Path $OutputPath) 'done'), '')
}
