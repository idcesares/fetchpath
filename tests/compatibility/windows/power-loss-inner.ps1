# FP-077 guest half. Run inside a persistent Windows 11 x64 VM whose download
# volume is NTFS. Do not run this on the owner's PC. Windows Sandbox does not
# qualify: it discards its disk on stop, and a mapped folder is the host volume.
#
# -SignalDirectory must survive the guest power cut when this script is run by
# hand (a host share). The host harness saves the signal itself before TurnOff.
# -DownloadDirectory must be on the guest volume under test.
# ArmCheckpoint and ArmPublication keep running until the VM loses power.
# After the next boot, run the matching Inspect phase.

[CmdletBinding()]
param(
    [ValidateSet('Serve', 'ArmCheckpoint', 'ArmPublication', 'InspectCheckpoint', 'InspectPublication')]
    [string] $Phase = 'ArmCheckpoint',
    [string] $SignalDirectory = 'C:\fp\out',
    [string] $DownloadDirectory = $(Join-Path $env:USERPROFILE 'Downloads\fp077')
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$fixtureByte = 0xA5
$checkpointTotal = 8MB
$publicationTotal = 128MB
$port = 37941

function Assert-Fp077([bool] $Condition, [string] $Message) {
    if (-not $Condition) { throw $Message }
}

if ([Environment]::UserName -eq 'WDAGUtilityAccount') {
    throw 'Windows Sandbox cannot keep a guest NTFS disk across an abrupt stop.'
}

function Get-VolumeFacts {
    $qualifier = Split-Path -Qualifier $DownloadDirectory
    if (-not $qualifier) { $qualifier = 'C:' }
    $disk = Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='$qualifier'"
    $system = Get-CimInstance Win32_ComputerSystem
    [ordered]@{
        drive = $qualifier
        fileSystem = $disk.FileSystem
        osCaption = (Get-CimInstance Win32_OperatingSystem).Caption
        osBuild = [Environment]::OSVersion.Version.ToString()
        model = $system.Model
        manufacturer = $system.Manufacturer
    }
}

function Write-Signal([string] $Name, $Body) {
    [System.IO.Directory]::CreateDirectory($SignalDirectory) | Out-Null
    $path = Join-Path $SignalDirectory $Name
    $json = ($Body | ConvertTo-Json -Depth 6 -Compress)
    $utf8 = New-Object System.Text.UTF8Encoding $false
    [System.IO.File]::WriteAllText($path, $json, $utf8)
    $stream = [System.IO.File]::Open($path, 'Open', 'Read', 'ReadWrite')
    $stream.Flush($true)
    $stream.Dispose()
    Write-Output "FP077-READY $Name"
}

function Invoke-Fp([string[]] $Arguments) {
    $exe = Join-Path $env:LOCALAPPDATA 'Fetchpath\fetchpath.exe'
    Assert-Fp077 (Test-Path -LiteralPath $exe) "Fetchpath is not installed at $exe"
    $text = (& $exe @Arguments 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw "fetchpath $($Arguments -join ' ') failed: $text" }
    return $text
}

function Get-Job([string] $JobId) {
    $jobs = @((Invoke-Fp @('ls', '--json') | ConvertFrom-Json).jobs)
    return @($jobs | Where-Object { $_.job_id -eq $JobId })[0]
}

function Get-Staging {
    @(Get-ChildItem -LiteralPath $DownloadDirectory -Force -Filter '.fetchpath-*.part' -ErrorAction SilentlyContinue)
}

function Get-CheckpointPhase {
    $files = @(Get-ChildItem -LiteralPath $DownloadDirectory -Force -Filter '.fetchpath-*.checkpoint.*' -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending)
    foreach ($file in $files) {
        $text = Get-Content -LiteralPath $file.FullName -Raw -ErrorAction SilentlyContinue
        if ($text -match '(?m)^phase=(publication_intent|downloading)$') { return $Matches[1] }
    }
    return $null
}

if ($Phase -eq 'Serve') {
    $config = Get-Content -LiteralPath (Join-Path $SignalDirectory 'serve.json') -Raw | ConvertFrom-Json
    $listener = [Net.HttpListener]::new()
    $listener.Prefixes.Add("http://127.0.0.1:$($config.port)/")
    $listener.Start()
    $buffer = [byte[]]::new(65536)
    for ($i = 0; $i -lt $buffer.Length; $i++) { $buffer[$i] = [byte] $config.byte }
    try {
        while ($listener.IsListening) {
            $context = $listener.GetContext()
            try {
                $total = [long] $config.total
                $start = 0L
                $end = $total - 1
                $range = $context.Request.Headers['Range']
                if ($range -match '^bytes=(\d+)-(\d*)$') {
                    $start = [long] $Matches[1]
                    if ($Matches[2]) { $end = [Math]::Min([long] $Matches[2], $end) }
                    $context.Response.StatusCode = 206
                    $context.Response.Headers['Content-Range'] = "bytes $start-$end/$total"
                }
                $context.Response.Headers['ETag'] = '"fp077"'
                $context.Response.Headers['Accept-Ranges'] = 'bytes'
                $context.Response.ContentLength64 = $end - $start + 1
                if ($context.Request.HttpMethod -ne 'HEAD') {
                    for ($offset = $start; $offset -le $end; ) {
                        $count = [int] [Math]::Min($buffer.Length, $end - $offset + 1)
                        $context.Response.OutputStream.Write($buffer, 0, $count)
                        $offset += $count
                        if ([int] $config.delayEvery -gt 0 -and ($offset % [int] $config.delayEvery) -lt $buffer.Length) {
                            Start-Sleep -Milliseconds 40
                        }
                    }
                }
            } catch { }
            finally { $context.Response.Close() }
        }
    } finally { $listener.Close() }
    return
}

function Start-Fixture([long] $Total, [int] $DelayEvery) {
    [System.IO.Directory]::CreateDirectory($SignalDirectory) | Out-Null
    @{ total = $Total; delayEvery = $DelayEvery; port = $port; byte = $fixtureByte } |
        ConvertTo-Json -Compress | Set-Content -LiteralPath (Join-Path $SignalDirectory 'serve.json') -Encoding utf8
    Start-Process -FilePath powershell.exe -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath,
        '-Phase', 'Serve', '-SignalDirectory', $SignalDirectory
    ) -WindowStyle Hidden | Out-Null
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        try {
            $probe = Invoke-WebRequest -Uri "http://127.0.0.1:$port/probe.bin" -Method Head -UseBasicParsing
            if ($probe.StatusCode -in 200, 206) { return }
        } catch { }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Fixture server did not start.'
}

function New-KeptFile {
    [System.IO.Directory]::CreateDirectory($DownloadDirectory) | Out-Null
    $path = Join-Path $DownloadDirectory 'kept.bin'
    [System.IO.File]::WriteAllBytes($path, [byte[]](0x4B, 0x45, 0x50, 0x54))
    return $path
}

function Wait-Cut([string] $Kind, [string] $JobId, [string] $Destination, [long] $Total) {
    $deadline = [DateTime]::UtcNow.AddSeconds(180)
    do {
        $part = Get-Staging | Select-Object -First 1
        $phase = Get-CheckpointPhase
        $length = if ($part) { $part.Length } else { 0 }
        $ready = $false
        if ($Kind -eq 'checkpoint') {
            $ready = ($phase -eq 'downloading' -and $length -gt 64KB -and $length -lt $Total -and -not (Test-Path -LiteralPath $Destination))
        } else {
            $ready = ($phase -eq 'publication_intent' -or ($length -eq $Total -and -not (Test-Path -LiteralPath $Destination)))
        }
        if ($ready) {
            $kept = Join-Path $DownloadDirectory 'kept.bin'
            Write-Signal "$(if ($Kind -eq 'checkpoint') { 'checkpoint-before.json' } else { 'publication-before.json' })" @{
                task = 'FP-077'
                cut = $Kind
                jobId = $JobId
                destination = $Destination
                expectedTotal = $Total
                fixtureByte = $fixtureByte
                stagingBytes = $length
                checkpointPhase = $phase
                keptPath = $kept
                keptSha256 = (Get-FileHash -LiteralPath $kept -Algorithm SHA256).Hash.ToLowerInvariant()
                destinationExisted = Test-Path -LiteralPath $Destination
                volume = Get-VolumeFacts
                durableModePromoted = $false
            }
            while ($true) { Start-Sleep -Seconds 5 }
        }
        $job = Get-Job $JobId
        if ($job -and $job.state -eq 'completed') {
            throw "The $Kind window closed before a power cut: the job completed."
        }
        Start-Sleep -Milliseconds 20
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Timed out waiting for the $Kind cut point."
}

function Invoke-Inspect([string] $SignalName) {
    $signalPath = Join-Path $SignalDirectory $SignalName
    Assert-Fp077 (Test-Path -LiteralPath $signalPath) "Pre-cut record is missing: $signalPath"
    $before = Get-Content -LiteralPath $signalPath -Raw | ConvertFrom-Json
    $keptHash = (Get-FileHash -LiteralPath $before.keptPath -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-Fp077 ($keptHash -eq $before.keptSha256) 'An existing file in the download folder changed.'
    $job = Get-Job $before.jobId
    Assert-Fp077 ($null -ne $job) 'The job is missing from the queue after boot.'
    $destinationExists = Test-Path -LiteralPath $before.destination
    $destinationLength = if ($destinationExists) { (Get-Item -LiteralPath $before.destination).Length } else { 0 }
    $complete = $job.state -eq 'completed'
    $bytesMatch = $false
    if ($destinationExists -and $destinationLength -eq [long] $before.expectedTotal) {
        $bytes = [System.IO.File]::ReadAllBytes($before.destination)
        $bytesMatch = (@($bytes | Where-Object { $_ -ne [byte] $before.fixtureByte }).Count -eq 0)
    }
    if ($complete) {
        Assert-Fp077 $bytesMatch 'A completed job published a missing, short, or unverified file.'
    } elseif ($destinationExists) {
        Assert-Fp077 $bytesMatch 'A partial destination was left in the download folder.'
    }
    $result = [ordered]@{
        task = 'FP-077'
        passed = $true
        cut = $before.cut
        jobState = $job.state
        destinationExisted = $destinationExists
        destinationLength = $destinationLength
        keptIntact = $true
        reportedComplete = $complete
        volume = Get-VolumeFacts
        durableModePromoted = $false
        limitation = 'One abrupt stop. This does not promote Fetchpath to a durable power-loss mode.'
    }
    Write-Signal ($(if ($before.cut -eq 'checkpoint') { 'checkpoint-after.json' } else { 'publication-after.json' })) $result
    $result | ConvertTo-Json -Depth 6
}

switch ($Phase) {
    ArmCheckpoint {
        $DownloadDirectory = Join-Path $DownloadDirectory 'checkpoint'
        $kept = New-KeptFile
        Start-Fixture $checkpointTotal (256KB)
        $destination = Join-Path $DownloadDirectory 'checkpoint.bin'
        $added = (Invoke-Fp @('add', "http://127.0.0.1:$port/checkpoint.bin", '--to', $DownloadDirectory, '--json') | ConvertFrom-Json)
        Wait-Cut 'checkpoint' $added.job.job_id $destination $checkpointTotal
    }
    ArmPublication {
        $DownloadDirectory = Join-Path $DownloadDirectory 'publication'
        if (-not (Test-Path -LiteralPath (Join-Path $DownloadDirectory 'kept.bin'))) { New-KeptFile | Out-Null }
        Start-Fixture $publicationTotal 0
        $destination = Join-Path $DownloadDirectory 'publish.bin'
        $added = (Invoke-Fp @('add', "http://127.0.0.1:$port/publish.bin", '--to', $DownloadDirectory, '--json') | ConvertFrom-Json)
        Wait-Cut 'publication' $added.job.job_id $destination $publicationTotal
    }
    InspectCheckpoint { Invoke-Inspect 'checkpoint-before.json' }
    InspectPublication { Invoke-Inspect 'publication-before.json' }
}
