[CmdletBinding()]
param(
    [string] $ChromePath
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
if (-not $ChromePath) {
    $ChromePath = Join-Path $repositoryRoot 'work\browser-spike\chrome-for-testing\chrome-win64\chrome.exe'
}
$ChromePath = [System.IO.Path]::GetFullPath($ChromePath)
if (-not (Test-Path -LiteralPath $ChromePath -PathType Leaf)) {
    throw "Chrome for Testing is unavailable: $ChromePath"
}

& cargo build -p fetchpath-desktop --bin fetchpath-browser-host --locked --offline
if ($LASTEXITCODE -ne 0) { throw 'The native host did not build.' }

$hostPath = [System.IO.Path]::GetFullPath((Join-Path $repositoryRoot 'target\debug\fetchpath-browser-host.exe'))
$workRoot = Join-Path $repositoryRoot 'work\browser-capture-runtime'
$workDir = Join-Path $workRoot 'session'
$hostManifest = Join-Path $workRoot 'com.fetchpath.browser.json'
[System.IO.Directory]::CreateDirectory($workRoot) | Out-Null
$manifest = Get-Content -Raw (Join-Path $repositoryRoot 'extensions\browser\native-host.chromium.json') | ConvertFrom-Json
$manifest.path = $hostPath
$manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $hostManifest -Encoding utf8

$registryPath = 'HKCU:\Software\Google\Chrome\NativeMessagingHosts\com.fetchpath.browser'
$hadKey = Test-Path -LiteralPath $registryPath
$previousValue = if ($hadKey) { Get-ItemPropertyValue -LiteralPath $registryPath -Name '(default)' -ErrorAction SilentlyContinue } else { $null }
try {
    New-Item -Path $registryPath -Force | Out-Null
    Set-ItemProperty -LiteralPath $registryPath -Name '(default)' -Value $hostManifest
    & node (Join-Path $PSScriptRoot 'run-chromium-runtime.mjs') `
        $ChromePath `
        (Join-Path $repositoryRoot 'extensions\browser') `
        $workDir `
        (Join-Path $repositoryRoot 'docs\development\evidence\browser\browser-capture.json')
    if ($LASTEXITCODE -ne 0) { throw 'The Chromium browser-capture runtime test failed.' }
} finally {
    if ($hadKey) {
        Set-ItemProperty -LiteralPath $registryPath -Name '(default)' -Value $previousValue
    } elseif (Test-Path -LiteralPath $registryPath) {
        Remove-Item -LiteralPath $registryPath -Force
    }
}
