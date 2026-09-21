$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$venv = Join-Path $repo 'work\fp016-venv'
$fixture = Join-Path $repo 'work\fp016-fixture'
$state = Join-Path $fixture 'state.json'

if (-not (Test-Path (Join-Path $venv 'Scripts\python.exe'))) {
    python -m venv $venv
}
$python = Join-Path $venv 'Scripts\python.exe'
& $python -m pip install --disable-pip-version-check -q -r (Join-Path $PSScriptRoot 'requirements.txt')
New-Item -ItemType Directory -Force -Path $fixture | Out-Null
Remove-Item -LiteralPath $state -ErrorAction SilentlyContinue

$server = Start-Process -FilePath $python -ArgumentList @(
    (Join-Path $PSScriptRoot 'fixture_servers.py'),
    '--state',
    $state
) -PassThru -WindowStyle Hidden

try {
    $deadline = (Get-Date).AddSeconds(30)
    while (-not (Test-Path $state)) {
        if ($server.HasExited) {
            throw "Protocol fixture server exited with code $($server.ExitCode)."
        }
        if ((Get-Date) -gt $deadline) {
            throw 'Timed out waiting for protocol fixtures.'
        }
        Start-Sleep -Milliseconds 100
    }
    $env:FETCHPATH_COMPATIBILITY_FIXTURE = $state
    Push-Location $repo
    try {
        cargo test -p fetchpath-http --test protocol_fixtures -- --ignored --nocapture
        if ($LASTEXITCODE -ne 0) {
            throw "Protocol compatibility tests failed with exit code $LASTEXITCODE."
        }
    } finally {
        Pop-Location
        Remove-Item Env:FETCHPATH_COMPATIBILITY_FIXTURE -ErrorAction SilentlyContinue
    }
} finally {
    if (-not $server.HasExited) {
        Stop-Process -Id $server.Id
        $server.WaitForExit()
    }
}
