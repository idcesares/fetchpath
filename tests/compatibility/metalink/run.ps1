$ErrorActionPreference = 'Stop'

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$fixture = Join-Path $repo 'work\fp019-fixture'
$state = Join-Path $fixture 'state.json'
$evidence = Join-Path $repo 'docs\development\evidence\metalink\fp019-mirror-matrix.json'

New-Item -ItemType Directory -Force -Path $fixture | Out-Null
Remove-Item -LiteralPath $state -ErrorAction SilentlyContinue

# Stdlib only: this harness needs no virtual environment and no downloads.
$server = Start-Process -FilePath 'python' -ArgumentList @(
    (Join-Path $PSScriptRoot 'mirror_servers.py'),
    '--state',
    $state
) -PassThru -WindowStyle Hidden

try {
    $deadline = (Get-Date).AddSeconds(30)
    while (-not (Test-Path $state)) {
        if ($server.HasExited) {
            throw "Metalink mirror fixture server exited with code $($server.ExitCode)."
        }
        if ((Get-Date) -gt $deadline) {
            throw 'Timed out waiting for the Metalink mirror fixtures.'
        }
        Start-Sleep -Milliseconds 100
    }

    $facts = Get-Content -LiteralPath $state -Raw | ConvertFrom-Json
    $env:FETCHPATH_METALINK_MIXED = $facts.mixed_metalink
    $env:FETCHPATH_METALINK_FINAL_ONLY = $facts.final_hash_only_metalink
    $env:FETCHPATH_METALINK_ALL_DAMAGED = $facts.all_damaged_metalink
    $env:FETCHPATH_METALINK_PAYLOAD = $facts.payload_path
    $env:FETCHPATH_METALINK_PAYLOAD_SHA256 = $facts.payload_sha256
    $env:FETCHPATH_METALINK_PIECE_LENGTH = $facts.piece_length
    $env:FETCHPATH_METALINK_PIECE_COUNT = $facts.piece_count
    $env:FETCHPATH_METALINK_DAMAGED_PIECE = $facts.damaged_piece
    $env:FETCHPATH_METALINK_WORK = Join-Path $fixture 'destinations'
    $env:FETCHPATH_METALINK_EVIDENCE = $evidence
    $env:FETCHPATH_METALINK_RECORDED = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')

    Push-Location $repo
    try {
        cargo test -p fetchpath-core --test metalink_mirrors -- --ignored --nocapture
        if ($LASTEXITCODE -ne 0) {
            throw "Metalink mirror tests failed with exit code $LASTEXITCODE."
        }
    } finally {
        Pop-Location
        foreach ($name in @(
                'FETCHPATH_METALINK_MIXED',
                'FETCHPATH_METALINK_FINAL_ONLY',
                'FETCHPATH_METALINK_ALL_DAMAGED',
                'FETCHPATH_METALINK_PAYLOAD',
                'FETCHPATH_METALINK_PAYLOAD_SHA256',
                'FETCHPATH_METALINK_PIECE_LENGTH',
                'FETCHPATH_METALINK_PIECE_COUNT',
                'FETCHPATH_METALINK_DAMAGED_PIECE',
                'FETCHPATH_METALINK_WORK',
                'FETCHPATH_METALINK_EVIDENCE',
                'FETCHPATH_METALINK_RECORDED')) {
            Remove-Item "Env:$name" -ErrorAction SilentlyContinue
        }
    }
    Write-Host "Recorded mirror evidence at $evidence"
} finally {
    if (-not $server.HasExited) {
        Stop-Process -Id $server.Id
        $server.WaitForExit()
    }
}
