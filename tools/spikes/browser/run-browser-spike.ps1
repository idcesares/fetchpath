[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../../..')).Path
$workDir = Join-Path $repoRoot 'work/browser-spike'
$sourceExtension = Join-Path $PSScriptRoot 'extension'
$runtimeExtension = Join-Path $workDir 'chrome-extension'
$profileDir = Join-Path $workDir 'chrome-profile'
$downloadDir = Join-Path $workDir 'downloads'
$ledgerPath = Join-Path $workDir 'host-ledger.jsonl'
$evidencePath = Join-Path $repoRoot 'docs/development/evidence/browser/browser-spike.json'
$manifestPath = Join-Path $workDir 'com.fetchpath.browser_spike.json'
$cargoManifest = Join-Path $PSScriptRoot 'native-host/Cargo.toml'
$cargoTarget = Join-Path $workDir 'native-host-target'
$hostExe = Join-Path $cargoTarget 'debug/fetchpath-browser-spike-host.exe'
$registryPath = 'HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.fetchpath.browser_spike'
$expectedRegistryPath = 'HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.fetchpath.browser_spike'

if ($registryPath -ne $expectedRegistryPath) {
  throw "Refusing unexpected registry target: $registryPath"
}

$cftRoot = Join-Path $workDir 'chrome-for-testing'
$chromePath = Join-Path $cftRoot 'chrome-win64/chrome.exe'
$cftArchive = Join-Path $workDir 'chrome-for-testing-win64.zip'
$cftMetadataUrl = 'https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json'
$cftDownloadUrl = $null
if (-not (Test-Path -LiteralPath $chromePath)) {
  $cftMetadata = Invoke-RestMethod -Uri $cftMetadataUrl
  $cftDownload = $cftMetadata.channels.Stable.downloads.chrome | Where-Object { $_.platform -eq 'win64' } | Select-Object -First 1
  if (-not $cftDownload) { throw 'Chrome for Testing stable win64 download was not published.' }
  $cftDownloadUrl = $cftDownload.url
  Invoke-WebRequest -Uri $cftDownloadUrl -OutFile $cftArchive
  if (Test-Path -LiteralPath $cftRoot) { Remove-Item -LiteralPath $cftRoot -Recurse -Force }
  Expand-Archive -LiteralPath $cftArchive -DestinationPath $cftRoot
}
if (-not (Test-Path -LiteralPath $chromePath)) {
  throw 'Chrome for Testing extraction did not produce chrome-win64/chrome.exe.'
}

New-Item -ItemType Directory -Force -Path $workDir, $runtimeExtension | Out-Null
Copy-Item -LiteralPath (Join-Path $sourceExtension 'background.js') -Destination $runtimeExtension -Force
Copy-Item -LiteralPath (Join-Path $sourceExtension 'probe.html') -Destination $runtimeExtension -Force
Copy-Item -LiteralPath (Join-Path $sourceExtension 'probe.js') -Destination $runtimeExtension -Force
Copy-Item -LiteralPath (Join-Path $sourceExtension 'manifest.chromium.json') -Destination (Join-Path $runtimeExtension 'manifest.json') -Force

$previousCargoTarget = $env:CARGO_TARGET_DIR
$env:CARGO_TARGET_DIR = $cargoTarget
try {
  cargo test --manifest-path $cargoManifest --locked
  if ($LASTEXITCODE -ne 0) { throw "native host tests failed with exit code $LASTEXITCODE" }
  cargo build --manifest-path $cargoManifest --locked
  if ($LASTEXITCODE -ne 0) { throw "native host build failed with exit code $LASTEXITCODE" }
} finally {
  $env:CARGO_TARGET_DIR = $previousCargoTarget
}

$hostManifest = [ordered]@{
  name = 'com.fetchpath.browser_spike'
  description = 'Fetchpath FP-006 isolated native messaging host'
  path = $hostExe
  type = 'stdio'
  allowed_origins = @('chrome-extension://lfikhkjdpjcjaboanknaabncpkbgoele/')
}
$hostManifestJson = $hostManifest | ConvertTo-Json -Depth 4
[System.IO.File]::WriteAllText($manifestPath, $hostManifestJson, [System.Text.UTF8Encoding]::new($false))

$hadRegistryKey = Test-Path -LiteralPath $registryPath
$previousRegistryValue = if ($hadRegistryKey) { (Get-Item -LiteralPath $registryPath).GetValue('') } else { $null }

try {
  New-Item -Path $registryPath -Force | Out-Null
  Set-Item -LiteralPath $registryPath -Value $manifestPath
  $env:FETCHPATH_BROWSER_VERSION = (Get-Item -LiteralPath $chromePath).VersionInfo.FileVersion
  $env:FETCHPATH_BROWSER_CHANNEL = 'Chrome for Testing stable'
  $env:FETCHPATH_BROWSER_SOURCE = if ($cftDownloadUrl) { $cftDownloadUrl } else { 'cached official Chrome for Testing archive' }
  node (Join-Path $PSScriptRoot 'run-browser-spike.mjs') $chromePath $runtimeExtension $profileDir $downloadDir $ledgerPath $evidencePath
  if ($LASTEXITCODE -ne 0) { throw "browser runtime failed with exit code $LASTEXITCODE" }
} finally {
  Remove-Item Env:\FETCHPATH_BROWSER_VERSION -ErrorAction SilentlyContinue
  Remove-Item Env:\FETCHPATH_BROWSER_CHANNEL -ErrorAction SilentlyContinue
  Remove-Item Env:\FETCHPATH_BROWSER_SOURCE -ErrorAction SilentlyContinue
  if ($hadRegistryKey) {
    Set-Item -LiteralPath $registryPath -Value $previousRegistryValue
  } elseif (Test-Path -LiteralPath $registryPath) {
    Remove-Item -LiteralPath $registryPath -Force
  }
}
