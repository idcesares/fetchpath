[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $PSScriptRoot))
$work = Join-Path $root 'work/media-spike'
$tools = Join-Path $work 'tools'
$fixtures = Join-Path $work 'fixtures'
$output = Join-Path $work 'output'
$evidence = Join-Path $root 'docs/development/evidence/media'
$archive = Join-Path $tools 'ffmpeg-9.0.1-full_build.zip'
$ytDlp = Join-Path $tools 'yt-dlp.exe'
$ffmpeg = Get-ChildItem (Join-Path $tools 'ffmpeg-9.0.1-full_build\bin\ffmpeg.exe') -ErrorAction SilentlyContinue

if (-not (Test-Path $ytDlp)) { throw 'Missing work/media-spike/tools/yt-dlp.exe. Run the documented acquisition commands first.' }
if (-not $ffmpeg) {
  if (-not (Test-Path $archive)) { throw 'Missing FFmpeg archive. Run the documented acquisition commands first.' }
  Expand-Archive -LiteralPath $archive -DestinationPath $tools -Force
  $ffmpeg = Get-ChildItem (Join-Path $tools 'ffmpeg-9.0.1-full_build\bin\ffmpeg.exe')
}
$ffmpegPath = $ffmpeg.FullName
$ffprobePath = Join-Path $ffmpeg.DirectoryName 'ffprobe.exe'

New-Item -ItemType Directory -Force -Path $fixtures, $output, $evidence | Out-Null
# The output directory is owned by this spike. `-Path` is intentional here so
# the scoped wildcard expands; `-LiteralPath` would preserve the `*` character.
Remove-Item -Path (Join-Path $output '*') -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path $fixtures 'hls'), (Join-Path $fixtures 'dash'), $output | Out-Null

# Locally generated, non-copyright fixture: moving test pattern plus synthetic tone.
& $ffmpegPath -hide_banner -loglevel error -y -f lavfi -i 'testsrc2=duration=12:size=640x360:rate=30' -f lavfi -i 'sine=frequency=880:duration=12' -c:v libx264 -b:v 1200k -pix_fmt yuv420p -c:a aac -b:a 128k -shortest (Join-Path $fixtures 'direct.mp4')
& $ffmpegPath -hide_banner -loglevel error -y -i (Join-Path $fixtures 'direct.mp4') -c copy -f hls -hls_time 2 -hls_playlist_type vod (Join-Path $fixtures 'hls/master.m3u8')
# FFmpeg resolves DASH segment names from its working directory. Keep every generated
# segment beside the manifest, never in the repository root.
Push-Location (Join-Path $fixtures 'dash')
try {
  & $ffmpegPath -hide_banner -loglevel error -y -i (Join-Path $fixtures 'direct.mp4') -map 0:v:0 -map 0:a:0 -c copy -f dash -seg_duration 2 -adaptation_sets 'id=0,streams=v id=1,streams=a' 'manifest.mpd'
}
finally {
  Pop-Location
}

& node (Join-Path $PSScriptRoot 'supervise-helper.mjs') $ytDlp $ffmpeg.DirectoryName $fixtures $output (Join-Path $evidence 'run.jsonl')
if ($LASTEXITCODE -ne 0) { throw "Supervised helper harness exited $LASTEXITCODE." }

& $ytDlp --version | Set-Content (Join-Path $evidence 'yt-dlp-version.txt')
& $ffmpegPath -version | Select-Object -First 1 | Set-Content (Join-Path $evidence 'ffmpeg-version.txt')
@("Python: $(& python --version)", "Node: $(& node --version)", "Windows: $([System.Environment]::OSVersion.VersionString)") | Set-Content (Join-Path $evidence 'environment.txt')
$hashInputs = @(
  (Join-Path $tools 'yt-dlp.exe')
  $archive
  (Join-Path $fixtures 'direct.mp4')
) + @(Get-ChildItem $output -File | Select-Object -ExpandProperty FullName)
$normalizedRoot = (Resolve-Path $root).Path.TrimEnd('\') + '\'
$hashInputs | ForEach-Object {
  $hash = Get-FileHash -LiteralPath $_ -Algorithm SHA256
  if (-not $hash.Path.StartsWith($normalizedRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Hash input escaped repository root: $($hash.Path)"
  }
  $relative = $hash.Path.Substring($normalizedRoot.Length).Replace('\', '/')
  "$($hash.Hash.ToLowerInvariant())  $relative"
} | Set-Content (Join-Path $evidence 'sha256.txt')
Get-ChildItem $output -File | Where-Object { $_.Extension -ne '.part' } | ForEach-Object { & $ffprobePath -v error -show_entries format=format_name,duration -of default=noprint_wrappers=1 $_.FullName } | Set-Content (Join-Path $evidence 'ffprobe-output.txt')
