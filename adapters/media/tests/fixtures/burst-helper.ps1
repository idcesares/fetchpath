# A stand-in for yt-dlp (FP-067 review): answers the inspection, then writes
# its whole output at once and exits, faster than any sampling can see.
if ($args -contains '--skip-download') {
    '{"title":"Burst","duration":1.0,"formats":[{"format_id":"18","vcodec":"avc1","acodec":"mp4a","height":360}]}'
    exit 0
}
$homeArg = $args | Where-Object { "$_" -like 'home:*' } | Select-Object -First 1
$folder = "$homeArg".Substring(5)
[System.IO.File]::WriteAllBytes((Join-Path $folder 'media.mp4'), [byte[]]::new(2MB))
exit 0
