# A stand-in for yt-dlp (FP-067): answers the inspection with one 360p
# format, then writes its output a little at a time, as a real download does,
# so a test can watch the bytes arrive and stop it part way.
if ($args -contains '--skip-download') {
    '{"title":"Growing","duration":1.0,"formats":[{"format_id":"18","vcodec":"avc1","acodec":"mp4a","height":360}]}'
    exit 0
}
$homeArg = $args | Where-Object { "$_" -like 'home:*' } | Select-Object -First 1
$folder = "$homeArg".Substring(5)
$part = Join-Path $folder 'media.mp4.part'
$chunk = [byte[]]::new(64KB)
$stream = [System.IO.File]::Open($part, 'Create', 'Write', 'ReadWrite')
try {
    for ($index = 0; $index -lt 60; $index++) {
        $stream.Write($chunk, 0, $chunk.Length)
        $stream.Flush()
        Start-Sleep -Milliseconds 100
    }
} finally {
    $stream.Close()
}
Move-Item -LiteralPath $part -Destination (Join-Path $folder 'media.mp4')
exit 0
