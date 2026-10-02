<#
Removes the two folders Fetchpath keeps its own data in (FP-100).

Run by the uninstaller, only when the person chose "Delete the application data"
(or passed /DELETEAPPDATA), after the engine and the app have stopped. NSIS's
own `RMDir /r` follows junctions: measured with NSIS 3.11, a junction inside the
tree sends the delete into the folder it points at. So the uninstaller does not
use it for these folders; this script does the removal instead.

Rules, each enforced below:
  - Only <APPDATA>\app.fetchpath.desktop and <LOCALAPPDATA>\app.fetchpath.desktop
    are ever removed: the exact leaf name, directly under the profile folder.
  - A root that is itself a reparse point (junction or symbolic link) is refused
    and left alone.
  - Inside a root, a reparse point is removed as a link and never entered, so
    whatever it points at is untouched.
  - A file or folder that cannot be removed is reported and the rest carries on.

Exit code: 0 everything removed or already absent, 3 something was refused or
could not be removed. -Roaming and -Local exist so tests can use scratch folders.
#>
param(
  [string]$Roaming = $env:APPDATA,
  [string]$Local = $env:LOCALAPPDATA
)
$ErrorActionPreference = 'Stop'
$leaf = 'app.fetchpath.desktop'
$script:problems = 0

function Test-Reparse([string]$path) {
  return [bool]([IO.File]::GetAttributes($path) -band [IO.FileAttributes]::ReparsePoint)
}

# The update hold is deleted last, so a client cannot start an engine that
# recreates data while the rest is being removed.
$holdName = 'engine-update-hold-v1'

function Remove-Tree([string]$dir, [bool]$isRoot) {
  foreach ($entry in [IO.Directory]::EnumerateFileSystemEntries($dir)) {
    if ($isRoot -and [IO.Path]::GetFileName($entry) -eq $holdName) { continue }
    try {
      $attributes = [IO.File]::GetAttributes($entry)
      $isDir = [bool]($attributes -band [IO.FileAttributes]::Directory)
      if ($attributes -band [IO.FileAttributes]::ReparsePoint) {
        # The link only. Directory.Delete on a junction removes the junction.
        if ($isDir) { [IO.Directory]::Delete($entry, $false) } else { [IO.File]::Delete($entry) }
      } elseif ($isDir) {
        Remove-Tree $entry $false
        [IO.Directory]::Delete($entry, $false)
      } else {
        [IO.File]::SetAttributes($entry, [IO.FileAttributes]::Normal)
        [IO.File]::Delete($entry)
      }
    } catch {
      $script:problems++
      Write-Output "Could not remove $entry ($($_.Exception.Message))"
    }
  }
}

foreach ($base in @($Roaming, $Local)) {
  if (-not $base -or -not [IO.Path]::IsPathRooted($base)) {
    $script:problems++
    Write-Output 'A profile folder is not set; skipped.'
    continue
  }
  $root = Join-Path ([IO.Path]::GetFullPath($base)) $leaf
  if (-not [IO.Directory]::Exists($root)) { Write-Output "Not present: $root"; continue }
  if (Test-Reparse $root) {
    $script:problems++
    Write-Output "Refused: $root is a junction or link, not a folder. Left alone."
    continue
  }
  try {
    Remove-Tree $root $true
    $hold = Join-Path $root $holdName
    if ([IO.File]::Exists($hold)) { [IO.File]::SetAttributes($hold, [IO.FileAttributes]::Normal); [IO.File]::Delete($hold) }
    [IO.Directory]::Delete($root, $false)
    Write-Output "Removed $root"
  } catch {
    $script:problems++
    Write-Output "Could not remove $root ($($_.Exception.Message))"
  }
}
if ($script:problems) { exit 3 }
exit 0
