# FP-077 host half. Abrupt power-off of a persistent Hyper-V guest during a
# checkpointed download and near publication, then boot and inspect.
#
# This never powers off the PC it is launched on. Windows Sandbox is not a
# substitute: wsb stop discards the guest disk. Without Hyper-V the script
# records that no power cut ran and exits 2.
#
#   powershell.exe -NoProfile -File tests/compatibility/windows/power-loss.ps1
#
# When a persistent Windows 11 x64 NTFS guest exists:
#   powershell.exe -NoProfile -File tests/compatibility/windows/power-loss.ps1 `
#     -VmName Fetchpath-power -TurnOff -GuestCredential (Get-Credential)

[CmdletBinding()]
param(
    [string] $VmName,
    [switch] $TurnOff,
    [PSCredential] $GuestCredential,
    [string] $OutputPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
if (-not $OutputPath) {
    $OutputPath = Join-Path $repositoryRoot 'docs\development\evidence\windows\fp077-power-loss.json'
}
$inner = Join-Path $PSScriptRoot 'power-loss-inner.ps1'
$os = Get-CimInstance Win32_OperatingSystem

function Write-Result($Body, [int] $Code) {
    $Body.recordedAt = [DateTime]::UtcNow.ToString('o')
    $Body.durableModePromoted = $false
    New-Item -ItemType Directory -Path (Split-Path -Parent $OutputPath) -Force | Out-Null
    $Body | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutputPath -Encoding utf8
    $Body | ConvertTo-Json -Depth 8
    exit $Code
}

$hyperv = [bool](Get-Command Get-VM -ErrorAction SilentlyContinue)
if (-not $TurnOff -or -not $hyperv -or -not $VmName) {
    $reason = if (-not $hyperv) {
        'Hyper-V VM cmdlets are not installed on this PC, so no guest was powered off.'
    } elseif (-not $VmName -or -not $TurnOff) {
        'No -VmName and -TurnOff were given, so no guest was powered off.'
    } else {
        'No power cut ran.'
    }
    Write-Result ([ordered]@{
        task = 'FP-077'
        passed = $false
        powerCutPerformed = $false
        hyperVModule = $hyperv
        windowsSandboxIsNotSufficient = $true
        host = [ordered]@{ caption = $os.Caption; build = $os.BuildNumber }
        reason = $reason
        limitation = 'Process kill and Windows Sandbox stop do not show power-loss recovery. A04 stays partial.'
    }) 2
}

if (-not $GuestCredential) { throw '-TurnOff requires -GuestCredential for PowerShell Direct.' }
$vm = Get-VM -Name $VmName
if ($vm.Generation -lt 1) { throw "VM $VmName was not found." }

function Enter-Guest([string] $Phase) {
    Invoke-Command -VMName $VmName -Credential $GuestCredential -ScriptBlock {
        param($Phase, $Script)
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $Script -Phase $Phase
    } -ArgumentList $Phase, 'C:\fp\power-loss-inner.ps1'
}

function Wait-GuestSignal([string] $Name) {
    $deadline = [DateTime]::UtcNow.AddSeconds(180)
    do {
        $json = Invoke-Command -VMName $VmName -Credential $GuestCredential -ScriptBlock {
            param($Name)
            $path = Join-Path 'C:\fp\out' $Name
            if (Test-Path -LiteralPath $path) { Get-Content -LiteralPath $path -Raw }
        } -ArgumentList $Name
        if ($json) { return $json }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Guest did not reach $Name."
}

if ($vm.State -ne 'Running') { Start-VM -Name $VmName }
Wait-VM -Name $VmName -For Heartbeat -Timeout 180 | Out-Null
$session = New-PSSession -VMName $VmName -Credential $GuestCredential
try {
    Invoke-Command -Session $session -ScriptBlock { New-Item -ItemType Directory -Path 'C:\fp' -Force | Out-Null }
    Copy-Item -ToSession $session -Path $inner -Destination 'C:\fp\power-loss-inner.ps1'
} finally { Remove-PSSession $session }

$saved = Join-Path $repositoryRoot 'work\fp077'
New-Item -ItemType Directory -Path $saved -Force | Out-Null
$cuts = @()
foreach ($round in @(
    @{ arm = 'ArmCheckpoint'; signal = 'checkpoint-before.json'; inspect = 'InspectCheckpoint'; after = 'checkpoint-after.json' },
    @{ arm = 'ArmPublication'; signal = 'publication-before.json'; inspect = 'InspectPublication'; after = 'publication-after.json' }
)) {
    Invoke-Command -VMName $VmName -Credential $GuestCredential -ScriptBlock {
        param($Phase)
        Start-Process powershell.exe -ArgumentList @(
            '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', 'C:\fp\power-loss-inner.ps1', '-Phase', $Phase
        ) -WindowStyle Hidden
    } -ArgumentList $round.arm | Out-Null
    $before = Wait-GuestSignal $round.signal
    Set-Content -LiteralPath (Join-Path $saved $round.signal) -Value $before -Encoding utf8
    Stop-VM -Name $VmName -TurnOff -Force
    Start-VM -Name $VmName
    Wait-VM -Name $VmName -For Heartbeat -Timeout 180 | Out-Null
    $session = New-PSSession -VMName $VmName -Credential $GuestCredential
    try {
        Invoke-Command -Session $session -ScriptBlock { New-Item -ItemType Directory -Path 'C:\fp\out' -Force | Out-Null }
        Copy-Item -ToSession $session -Path (Join-Path $saved $round.signal) -Destination (Join-Path 'C:\fp\out' $round.signal)
        $after = Invoke-Command -Session $session -ScriptBlock {
            param($Phase)
            & powershell.exe -NoProfile -ExecutionPolicy Bypass -File 'C:\fp\power-loss-inner.ps1' -Phase $Phase
        } -ArgumentList $round.inspect
    } finally { Remove-PSSession $session }
    $cuts += [ordered]@{ cut = $round.arm; before = ($before | ConvertFrom-Json); after = $after }
}

$passed = @($cuts | Where-Object { -not $_.after.passed }).Count -eq 0
Write-Result ([ordered]@{
    task = 'FP-077'
    passed = [bool] $passed
    powerCutPerformed = $true
    turnOff = $true
    vm = [ordered]@{ name = $vm.Name; generation = $vm.Generation }
    host = [ordered]@{ caption = $os.Caption; build = $os.BuildNumber }
    cuts = $cuts
    limitation = 'One abrupt stop at each point. This does not promote Fetchpath to a durable power-loss mode.'
}) $(if ($passed) { 0 } else { 1 })
