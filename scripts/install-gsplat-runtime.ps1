[CmdletBinding()]
param(
  [string]$InstallRoot
)

$ErrorActionPreference = 'Stop'

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  $arguments = "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
  if ($InstallRoot) {
    $arguments += " -InstallRoot `"$InstallRoot`""
  }
  $elevated = Start-Process -FilePath 'powershell.exe' -Verb RunAs -Wait -PassThru -ArgumentList $arguments
  exit $elevated.ExitCode
}

function Resolve-InstallRoot {
  param([string]$RequestedRoot)
  if ($RequestedRoot) {
    return [System.IO.Path]::GetFullPath($RequestedRoot)
  }

  $candidates = @(
    (Join-Path $env:ProgramFiles 'OOOSplat'),
    (Join-Path $env:LOCALAPPDATA 'OOOSplat')
  ) | Where-Object { Test-Path -LiteralPath (Join-Path $_ 'resources') -PathType Container }
  if ($candidates.Count -eq 1) {
    return $candidates[0]
  }
  if ($candidates.Count -gt 1) {
    throw 'Multiple OOOSplat installations were found. Use -InstallRoot to select one.'
  }
  throw 'OOOSplat installation was not found. Install the base package first or use -InstallRoot.'
}

$packageRoot = $PSScriptRoot
$runtimeSource = Join-Path $packageRoot 'gsplat'
$python = Join-Path $runtimeSource 'python\Scripts\python.exe'
$adapter = Join-Path $runtimeSource 'adapter\train_adapter.py'
if (-not (Test-Path -LiteralPath $python -PathType Leaf) -or -not (Test-Path -LiteralPath $adapter -PathType Leaf)) {
  throw 'The runtime package is incomplete: Python CUDA runtime or gsplat adapter is missing.'
}

$root = Resolve-InstallRoot $InstallRoot
$engines = @(
  (Join-Path $root 'engines'),
  (Join-Path $root 'resources\engines')
) | Where-Object { Test-Path -LiteralPath $_ -PathType Container } | Select-Object -First 1
if (-not $engines) {
  throw "Installation is incomplete: no engines directory was found under $root."
}

$stamp = Get-Date -Format 'yyyyMMddHHmmss'
$stage = Join-Path $engines ".gsplat-install-$stamp"
$target = Join-Path $engines 'gsplat'
$backup = Join-Path $engines "gsplat-backup-$stamp"
Copy-Item -LiteralPath $runtimeSource -Destination $stage -Recurse

try {
  if (Test-Path -LiteralPath $target) {
    Move-Item -LiteralPath $target -Destination $backup
  }
  Move-Item -LiteralPath $stage -Destination $target
} catch {
  if (Test-Path -LiteralPath $stage) {
    Write-Warning "Installation did not complete. Temporary directory was retained: $stage"
  }
  if (-not (Test-Path -LiteralPath $target) -and (Test-Path -LiteralPath $backup)) {
    Move-Item -LiteralPath $backup -Destination $target
  }
  throw
}

Write-Host "gsplat CUDA runtime installed to $target"
if (Test-Path -LiteralPath $backup) {
  Write-Host "Previous runtime was retained at $backup"
}
