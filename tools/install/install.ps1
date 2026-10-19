<#
.SYNOPSIS
  Headless Fetchpath installation for a person or an agent on a remote PC.
.DESCRIPTION
  Downloads a published Fetchpath setup, checks it against the release's
  SHA256SUMS.txt (this proves the download matches what was published, not who
  published it), and runs it silently for the current Windows user. Nothing is
  shown and nothing is asked. By default it installs the command line and the
  agent (MCP) command, no desktop window. It creates no agent-host connection,
  grants no folder access and downloads no media tools unless you ask.
.PARAMETER Version
  A release tag such as v0.3.0. Default: the latest release.
.PARAMETER Components
  Comma-separated: desktop, cli, mcp, browser, torrent, or full. Default cli,mcp.
.PARAMETER AgentHosts
  codex and/or claude-code, to connect for this Windows user. Needs mcp.
.PARAMETER MediaTools
  Also run `fetchpath tools install --yes` (yt-dlp and ffmpeg, checksum-pinned).
.PARAMETER SetupFile
  Use this already-downloaded setup instead of downloading (still not trusted
  without -Sha256).
.PARAMETER Sha256
  Expected SHA-256 of the setup, from a source you trust. Required with -SetupFile.
.PARAMETER InstallDir
  Install folder. Default: Setup's per-user folder.
.EXAMPLE
  .\install.ps1 -AgentHosts claude-code -MediaTools
#>
[CmdletBinding()]
param(
  [string]$Version = 'latest',
  [string]$Components = 'cli,mcp',
  [string[]]$AgentHosts = @(),
  [switch]$MediaTools,
  [string]$SetupFile,
  [string]$Sha256,
  [string]$InstallDir
)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$repo = 'idcesares/fetchpath'

# Not Write-Error: with ErrorActionPreference=Stop it would throw and lose the exit code.
function Fail($message, $code = 1) { [Console]::Error.WriteLine("fetchpath install: $message"); exit $code }

if ($Components -notmatch '^(full|[a-z]+(,[a-z]+)*)\z') { Fail 'Components must be names such as cli,mcp.' 2 }
if ($AgentHosts.Count -gt 0 -and $Components -ne 'full' -and $Components -notmatch '\bmcp\b') {
  Fail '-AgentHosts needs the mcp component.' 2
}
if ($Version -ne 'latest' -and $Version -notmatch '^v?\d+\.\d+\.\d+(-[\w.]+)?\z') { Fail 'Version must look like v0.3.0.' 2 }
if ($Sha256 -and $Sha256 -notmatch '^[0-9a-fA-F]{64}\z') { Fail '-Sha256 must be 64 hex characters.' 2 }
if ($Sha256 -and -not $SetupFile) { Fail '-Sha256 is only used with -SetupFile; a download is checked against SHA256SUMS.txt.' 2 }
if ($InstallDir -and ($InstallDir -match '["\r\n]' -or -not [IO.Path]::IsPathRooted($InstallDir))) { Fail '-InstallDir must be a full path without quotes.' 2 }
foreach ($h in $AgentHosts) { if ($h -notin 'codex', 'claude-code') { Fail "Unknown agent host '$h'." 2 } }

# Where Fetchpath is installed for this user: -InstallDir, else the folder setup
# recorded when it installed, else Setup's per-user default.
function Get-InstallFolder {
  if ($InstallDir) { return $InstallDir }
  $key = Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Fetchpath' -ErrorAction SilentlyContinue
  if ($key -and $key.InstallLocation) { return $key.InstallLocation.Trim('"') }
  return Join-Path $env:LOCALAPPDATA 'Fetchpath'
}

# Each run gets its own empty folder: nothing downloaded is run from where it landed beside other files.
$work = Join-Path ([IO.Path]::GetTempPath()) ("fetchpath-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null

try {
  if ($SetupFile) {
    if (-not $Sha256) { Fail '-SetupFile needs -Sha256 from a source you trust.' 2 }
    # Hash and run one private copy, not a file others may be able to replace.
    $setup = Join-Path $work (Split-Path $SetupFile -Leaf)
    Copy-Item -LiteralPath $SetupFile -Destination $setup
    $expected = $Sha256.ToLowerInvariant()
  } else {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $api = if ($Version -eq 'latest') { "https://api.github.com/repos/$repo/releases/latest" }
           else { "https://api.github.com/repos/$repo/releases/tags/$Version" }
    $release = Invoke-RestMethod -Uri $api -Headers @{ 'User-Agent' = 'fetchpath-install' }
    $base = "https://github.com/$repo/releases/download/"
    $exes = @($release.assets | Where-Object { $_.name -match '^Fetchpath_\d+\.\d+\.\d+(-[\w.]+)?_x64-setup\.exe\z' })
    $sumsList = @($release.assets | Where-Object { $_.name -eq 'SHA256SUMS.txt' })
    if ($exes.Count -ne 1 -or $sumsList.Count -ne 1) { Fail "Release $($release.tag_name) must have exactly one setup and one SHA256SUMS.txt." 3 }
    $exe = $exes[0]
    $sums = $sumsList[0]
    if (-not $exe.browser_download_url.StartsWith($base) -or -not $sums.browser_download_url.StartsWith($base)) { Fail 'Release assets are not hosted by this repository.' 3 }
    $setup = Join-Path $work $exe.name
    $sumsFile = Join-Path $work 'SHA256SUMS.txt'
    Invoke-WebRequest -Uri $exe.browser_download_url -OutFile $setup -UseBasicParsing
    Invoke-WebRequest -Uri $sums.browser_download_url -OutFile $sumsFile -UseBasicParsing
    $line = Get-Content -LiteralPath $sumsFile | Where-Object { $_ -match ('^[0-9a-fA-F]{64}\s+\*?' + [regex]::Escape($exe.name) + '$') } | Select-Object -First 2
    if (@($line).Count -ne 1) { Fail "SHA256SUMS.txt does not list $($exe.name)." 3 }
    $expected = ($line -split '\s+')[0].ToLowerInvariant()
  }

  $actual = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($actual -ne $expected) { Fail "Checksum mismatch for $(Split-Path $setup -Leaf). Nothing was installed." 3 }

  # An existing install keeps its components unless -Components was given, so
  # a rerun never removes a desktop app or browser bridge it did not mention.
  $existing = Test-Path -LiteralPath (Join-Path (Get-InstallFolder) 'fetchpath.exe')
  $setupArgs = @('/S')
  if ($PSBoundParameters.ContainsKey('Components') -or -not $existing) { $setupArgs += "/COMPONENTS=$Components" }
  if ($AgentHosts.Count -gt 0) { $setupArgs += "/AGENTHOSTS=$($AgentHosts -join ',')" }
  if ($InstallDir) { $setupArgs += "/D=$InstallDir" }   # /D must be last
  $process = Start-Process -FilePath $setup -ArgumentList $setupArgs -Wait -PassThru
  if ($process.ExitCode -ne 0) {
    $log = Join-Path $env:TEMP 'fetchpath-install.log'
    $why = if ((Test-Path $log) -and (Get-Item $log).LastWriteTime -ge $process.StartTime) { (Get-Content $log -Tail 5) -join ' | ' } else { 'no log' }
    # 10: unusable /COMPONENTS; 11: agent connection failed (Fetchpath stays installed).
    Fail "Setup exited with code $($process.ExitCode). $why" $process.ExitCode
  }

  # This session's PATH predates the install, so look in the install folder first.
  $folder = Get-InstallFolder
  $cliPath = Join-Path $folder 'fetchpath.exe'
  if (-not (Test-Path -LiteralPath $cliPath)) {
    if ($MediaTools) { Fail 'Media tools need the cli component.' 2 }
    Write-Output "Installed. The fetchpath command is not part of these components."
  } else {
    if ($MediaTools) {
      & $cliPath tools install --yes
      if ($LASTEXITCODE -ne 0) { Fail 'Media tools were not set up.' $LASTEXITCODE }
    }
    Write-Output "Installed: $cliPath"
    if ($Components -eq 'full' -or $Components -match '\bmcp\b') {
      & $cliPath agent-setup status --json
    }
  }
} finally {
  Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}
