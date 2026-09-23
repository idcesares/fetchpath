<#
Adds or removes one folder in the per-user PATH (FP-037).

Run by the installer hooks. NSIS cannot do this safely: its registry reads
return an EMPTY string for any value longer than NSIS_MAX_STRLEN (1024), so a
long PATH looks empty and writing it back destroys it. This script reads and
writes the raw value through the registry API instead, with no length limit,
and keeps %VARIABLE% references unexpanded and the value's registry type.

Safety rules, each enforced below:
  - Nothing is written unless the current value was read successfully.
  - Only entries equal to -Dir (ignoring case and a trailing backslash) are
    ever added or removed; every other character of PATH is kept as it was.
  - An add never shortens PATH, and a remove never empties a PATH that held
    anything besides -Dir.

  -Current VALUE -DryRun   prints the value that would be written, for tests.
#>
param(
  [Parameter(Mandatory)][ValidateSet('Add', 'Remove')][string]$Action,
  [Parameter(Mandatory)][string]$Dir,
  [string]$Current,
  [switch]$DryRun
)
$ErrorActionPreference = 'Stop'

function Get-NextPath([string]$current, [string]$dir, [string]$action) {
  $target = $dir.TrimEnd('\')
  $parts = if ($current) { $current -split ';' } else { @() }
  $found = @($parts | Where-Object { $_ -and $_.TrimEnd('\') -ieq $target })
  if ($action -eq 'Add') {
    if ($found.Count) { return $null }
    if (-not $current) { return $dir }
    if ($current.EndsWith(';')) { return "$current$dir" }
    return "$current;$dir"
  }
  if (-not $found.Count) { return $null }
  $kept = @($parts | Where-Object { -not ($_ -and $_.TrimEnd('\') -ieq $target) })
  # "a;dir" -> "a", and a trailing empty segment left by removing the last entry
  # is dropped so the result does not gain a stray separator.
  if ($kept.Count -and $kept[-1] -eq '' -and -not $current.EndsWith(';')) { $kept = $kept[0..($kept.Count - 2)] }
  return ($kept -join ';')
}

function Assert-Safe([string]$current, [string]$next, [string]$action, [string]$dir) {
  if ($action -eq 'Add' -and $next.Length -le $current.Length) { throw 'refusing to shorten PATH while adding' }
  if ($action -eq 'Remove') {
    $others = @(($current -split ';') | Where-Object { $_ -and $_.TrimEnd('\') -ine $dir.TrimEnd('\') })
    if ($others.Count -and -not $next) { throw 'refusing to empty PATH' }
    if ($next.Length -ge $current.Length) { throw 'remove did not shorten PATH' }
  }
}

if ($DryRun) {
  $next = Get-NextPath $Current $Dir $Action
  if ($null -ne $next) { Assert-Safe $Current $next $Action $Dir }
  if ($null -eq $next) { '<unchanged>' } else { $next }
  exit 0
}

$key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
if (-not $key) { throw 'HKCU\Environment is not writable' }
try {
  $exists = $key.GetValueNames() -contains 'Path'
  $current = if ($exists) { [string]$key.GetValue('Path', '', 'DoNotExpandEnvironmentNames') } else { '' }
  $kind = if ($exists) { $key.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
  $next = Get-NextPath $current $Dir $Action
  if ($null -eq $next) { exit 0 }
  Assert-Safe $current $next $Action $Dir
  $key.SetValue('Path', $next, $kind)
} finally {
  $key.Close()
}
