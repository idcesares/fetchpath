# FP-103 G2: invoked only in a disposable Windows Sandbox by the outer harness.
[CmdletBinding()]
param([ValidateSet('Setup','ServeLaunch','Serve','Before','LogoffProbe','LoginProbe','After')][string]$Phase = 'Setup')
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$out = 'C:\fp\out'
$exe = Join-Path $env:LOCALAPPDATA 'Fetchpath\fetchpath.exe'
function Assert-G2([bool]$Condition,[string]$Message) { if (-not $Condition) { throw $Message } }
function Invoke-Fp([string[]]$Arguments) {
    $text = (& $exe @Arguments 2> "$out\cli-error.txt" | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw "Fetchpath command failed: $($Arguments[0]); $(Get-Content "$out\cli-error.txt" -Raw -ErrorAction SilentlyContinue)" }
    return $text
}
function Engine-Process {
    return @(Get-CimInstance Win32_Process -Filter "Name='fetchpath.exe'" | Where-Object {
        $_.ExecutablePath -eq $exe -and $_.CommandLine -match '\sengine(?:\s|$)'
    })
}
function Logon-Sid { return (& whoami.exe /logonid | Out-String).Trim() }
try {
    switch ($Phase) {
        Setup {
            if (Test-Path "$out\setup.json") { break }
            $key = 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy'
            $sac = Get-ItemProperty -LiteralPath $key -Name VerifiedAndReputablePolicyState -ErrorAction SilentlyContinue
            if ($sac -and $sac.VerifiedAndReputablePolicyState -ne 0) {
                Set-ItemProperty -LiteralPath $key -Name VerifiedAndReputablePolicyState -Value 0 -Type DWord
                $refresh = Start-Process CiTool.exe -ArgumentList '--refresh','--json' -PassThru -WindowStyle Hidden
                Assert-G2 ($refresh.WaitForExit(60000)) 'Sandbox SAC refresh timed out'
            }
            $setup = Join-Path $env:TEMP 'fp103-g2-setup.exe'
            Copy-Item -LiteralPath 'C:\fp\in\setup.exe' -Destination $setup
            $installer = Start-Process $setup -ArgumentList '/S','/COMPONENTS=desktop,cli' -PassThru -WindowStyle Hidden
            Assert-G2 ($installer.WaitForExit(600000)) 'Sandbox install timed out'
            Assert-G2 ($installer.ExitCode -eq 0) 'Sandbox install failed'
            $version = Invoke-Fp @('--version')
            Assert-G2 ($version -match '0\.2\.0') 'Wrong packaged CLI version'
            [IO.File]::WriteAllText("$out\setup.json",(@{ version=$version; sandboxSacState=if ($sac) { $sac.VerifiedAndReputablePolicyState } else { 0 } } | ConvertTo-Json))
        }
        LogoffProbe {
            $before=Get-Content "$out\before.json" -Raw | ConvertFrom-Json
            if (-not (Get-Process -Id $before.enginePid -ErrorAction SilentlyContinue)) {
                [IO.File]::WriteAllText("$out\logoff-ready",'ready')
            }
        }
        LoginProbe {
            $before=Get-Content "$out\before.json" -Raw | ConvertFrom-Json
            $sid=Logon-Sid
            if ($sid -and $sid -ne $before.logonSid) { [IO.File]::WriteAllText("$out\login-ready.txt",$sid) }
        }
        ServeLaunch {
            # SYSTEM keeps this loopback fixture alive across the guest user's logoff.
            Start-Process powershell.exe -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','C:\fp\in\sign-in-lifecycle-inner.ps1','-Phase','Serve' -WindowStyle Hidden
        }
        Serve {
            $listener = [Net.HttpListener]::new()
            $listener.Prefixes.Add('http://127.0.0.1:37931/')
            $listener.Start()
            [IO.File]::WriteAllText("$out\server-ready",'')
            $total = 2097152
            $buffer = [byte[]]::new(8192)
            for ($i=0;$i -lt $buffer.Length;$i++) { $buffer[$i]=0x5a }
            try {
                while ($true) {
                    $context = $listener.GetContext()
                    try {
                        $start = 0; $end = $total-1
                        $range = $context.Request.Headers['Range']
                        if ($range -match '^bytes=(\d+)-(\d*)$') {
                            $start = [int]$Matches[1]
                            if ($Matches[2]) { $end = [Math]::Min([int]$Matches[2],$end) }
                            $context.Response.StatusCode=206
                            $context.Response.Headers['Content-Range']="bytes $start-$end/$total"
                        }
                        $context.Response.Headers['ETag']='"fp103-g2"'
                        $context.Response.Headers['Accept-Ranges']='bytes'
                        $context.Response.ContentLength64=$end-$start+1
                        if (Test-Path "$out\resume") { [IO.File]::AppendAllText("$out\after-ranges.txt","$start`n") }
                        if ($context.Request.HttpMethod -ne 'HEAD') {
                            for ($offset=$start;$offset -le $end;) {
                                while ($offset -ge 524288 -and -not (Test-Path "$out\resume")) { Start-Sleep -Milliseconds 100 }
                                $count=[Math]::Min($buffer.Length,$end-$offset+1)
                                $context.Response.OutputStream.Write($buffer,0,$count)
                                $offset += $count
                                Start-Sleep -Milliseconds 8
                            }
                        }
                    } catch { } finally { $context.Response.Close() }
                }
            } finally { $listener.Close() }
        }
        Before {
            Invoke-Fp @('hub','on') | Out-Null
            $status=(Invoke-Fp @('engine','status','--json') | ConvertFrom-Json).status
            $desktop=Start-Process (Join-Path (Split-Path $exe) 'fetchpath-desktop.exe') -PassThru -WindowStyle Hidden
            Start-Sleep -Seconds 5
            Assert-G2 (-not $desktop.HasExited) 'Desktop did not start'
            $pidBefore=@(Engine-Process)[0].ProcessId
            Stop-Process -Id $desktop.Id
            Start-Sleep -Seconds 2
            Assert-G2 (@(Engine-Process).Count -eq 1 -and @(Engine-Process)[0].ProcessId -eq $pidBefore) 'Desktop exit stopped the always-on engine'
            $folder=Join-Path $env:USERPROFILE 'Downloads'
            New-Item -ItemType Directory -Path $folder -Force | Out-Null
            $added=Invoke-Fp @('add','http://127.0.0.1:37931/g2.bin','--to',$folder,'--json') | ConvertFrom-Json
            $id=$added.job.job_id
            $deadline=[DateTime]::UtcNow.AddSeconds(30)
            do {
                $jobs=(Invoke-Fp @('ls','--json') | ConvertFrom-Json).jobs
                $job=@($jobs | Where-Object job_id -eq $id)[0]
                if ($job.progress.bytes_received -ge 131072) { break }
                Start-Sleep -Milliseconds 300
            } while ([DateTime]::UtcNow -lt $deadline)
            Assert-G2 ($job.progress.bytes_received -ge 131072) 'Fixture never became a partial download'
            Invoke-Fp @('pause',$id) | Out-Null
            $deadline=[DateTime]::UtcNow.AddSeconds(15)
            do {
                $job=@((Invoke-Fp @('ls','--json') | ConvertFrom-Json).jobs | Where-Object job_id -eq $id)[0]
                if ($job.state -eq 'paused') { break }
                Start-Sleep -Milliseconds 200
            } while ([DateTime]::UtcNow -lt $deadline)
            Assert-G2 ($job.state -eq 'paused') 'Could not checkpoint the partial download'
            Invoke-Fp @('resume',$id) | Out-Null
            $before=@{ logonSid=Logon-Sid; instanceId=$status.instance.id; engineVersion=$status.engine_version; executableHash=(Get-FileHash -LiteralPath $exe).Hash; enginePid=$pidBefore; jobId=$id; destination=(Join-Path $folder 'g2.bin'); desktopExitPassed=$true }
            [IO.File]::WriteAllText("$out\before.json",($before | ConvertTo-Json))
        }
        After {
            $before=Get-Content "$out\before.json" -Raw | ConvertFrom-Json
            Assert-G2 ((Logon-Sid) -ne $before.logonSid) 'A reconnect did not create a new Windows logon'
            # Observe startup before any client command which could launch an engine.
            $deadline=[DateTime]::UtcNow.AddSeconds(75)
            do {
                $processes=@(Engine-Process)
                if ($processes.Count -eq 1) { break }
                Start-Sleep -Milliseconds 500
            } while ([DateTime]::UtcNow -lt $deadline)
            Assert-G2 ($processes.Count -eq 1) 'Windows sign-in did not start the engine automatically'
            Assert-G2 ($processes[0].ProcessId -ne $before.enginePid) 'The original engine was not replaced across logoff'
            $pidAfter=$processes[0].ProcessId
            # Let the old fixture connection unwind before metadata recovery.
            # Automatic startup has already been observed without a client.
            [IO.File]::WriteAllText("$out\resume",'')
            $readyDeadline=[DateTime]::UtcNow.AddSeconds(30)
            do {
                try {
                    $status=(Invoke-Fp @('engine','status','--json') | ConvertFrom-Json).status
                    break
                } catch {
                    if ([DateTime]::UtcNow -ge $readyDeadline) { throw }
                    Start-Sleep -Milliseconds 300
                }
            } while ($true)
            Assert-G2 ($status.instance.id -eq $before.instanceId) 'Sign-in changed the persisted instance identity'
            Assert-G2 ($status.engine_version -eq $before.engineVersion -and (Get-FileHash -LiteralPath $exe).Hash -eq $before.executableHash) 'The engine executable changed across sign-in'
            Assert-G2 ((Invoke-Fp @('--version')) -match '0\.2\.0') 'Wrong packaged CLI version after sign-in'
            $deadline=[DateTime]::UtcNow.AddSeconds(75)
            do {
                $job=@((Invoke-Fp @('ls','--json') | ConvertFrom-Json).jobs | Where-Object job_id -eq $before.jobId)[0]
                if ($job.state -eq 'completed') { break }
                Start-Sleep -Milliseconds 500
            } while ([DateTime]::UtcNow -lt $deadline)
            Assert-G2 ($job.state -eq 'completed') 'Persisted job did not complete after Windows sign-in'
            Assert-G2 (@(Engine-Process).Count -eq 1 -and @(Engine-Process)[0].ProcessId -eq $pidAfter) 'A client replaced the automatically started engine'
            $bytes=[IO.File]::ReadAllBytes($before.destination)
            Assert-G2 ($bytes.Length -eq 2097152 -and @($bytes | Where-Object { $_ -ne 0x5a }).Count -eq 0) 'Recovered bytes differ from the fixture'
            $ranges=@(Get-Content "$out\after-ranges.txt" | ForEach-Object { [int]$_ })
            Assert-G2 (@($ranges | Where-Object { $_ -gt 0 }).Count -gt 0) 'No checkpoint range was resumed after sign-in'
            Start-Sleep -Seconds 65
            Assert-G2 (@(Engine-Process).Count -eq 1 -and @(Engine-Process)[0].ProcessId -eq $pidAfter) 'Automatically started engine exited after the last download and client'
            $result=@{ passed=$true; environment='Windows Sandbox'; osBuild=[Environment]::OSVersion.Version.ToString(); version='0.2.0'; newLogonVerified=$true; desktopExitPassed=$before.desktopExitPassed; automaticEngineStart=$true; sameAutomaticEngineThroughIdle=$true; stableInstance=$true; resumedRange=($ranges | Measure-Object -Maximum).Maximum; savedBytes=$bytes.Length; bytesVerified=$true; idlePastDefaultGrace=$true; limitation='Actual guest sign-out/sign-in; no reboot or power-loss test' }
            [IO.File]::WriteAllText("$out\result.json",($result | ConvertTo-Json))
            Invoke-Fp @('hub','off') | Out-Null
            Invoke-Fp @('engine','stop') | Out-Null
        }
    }
} catch {
    [IO.File]::WriteAllText("$out\error.json",(@{ phase=$Phase; error=$_.Exception.Message; passed=$false } | ConvertTo-Json))
    exit 1
}
