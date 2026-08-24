[CmdletBinding()]
param(
  [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (-not $OutputDirectory) {
  $OutputDirectory = Join-Path $workspace '.tmp\runtime-packages'
}
$runtimeRoot = Join-Path $workspace 'engines\gsplat'
$python = Join-Path $runtimeRoot 'python\Scripts\python.exe'
$adapter = Join-Path $runtimeRoot 'adapter\train_adapter.py'
if (-not (Test-Path -LiteralPath $python -PathType Leaf) -or -not (Test-Path -LiteralPath $adapter -PathType Leaf)) {
  throw 'Local gsplat runtime is incomplete; cannot create the companion runtime archive.'
}

$tar = Get-Command tar.exe -ErrorAction Stop
$version = (Get-Content -LiteralPath (Join-Path $workspace 'package.json') -Raw | ConvertFrom-Json).version
$output = [System.IO.Path]::GetFullPath($OutputDirectory)
$stage = Join-Path $output "gsplat-runtime-$version"
$archive = Join-Path $output "OOOSplat-gsplat-runtime-$version.zip"
New-Item -ItemType Directory -Force -Path $output | Out-Null
if (Test-Path -LiteralPath $stage) { throw "Staging directory already exists. Review and remove it manually: $stage" }
if (Test-Path -LiteralPath $archive) { throw "Output archive already exists. Review and remove it manually: $archive" }

New-Item -ItemType Directory -Path $stage | Out-Null
New-Item -ItemType Directory -Path (Join-Path $stage 'gsplat') | Out-Null
Copy-Item -LiteralPath (Join-Path $runtimeRoot 'adapter') -Destination (Join-Path $stage 'gsplat\adapter') -Recurse
Copy-Item -LiteralPath (Join-Path $runtimeRoot 'python') -Destination (Join-Path $stage 'gsplat\python') -Recurse
if (Test-Path -LiteralPath (Join-Path $runtimeRoot 'LICENSES')) {
  Copy-Item -LiteralPath (Join-Path $runtimeRoot 'LICENSES') -Destination (Join-Path $stage 'gsplat\LICENSES') -Recurse
}
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'install-gsplat-runtime.ps1') -Destination (Join-Path $stage 'install-gsplat-runtime.ps1')

Push-Location $output
try {
  & $tar.Source -a -c -f $archive (Split-Path $stage -Leaf)
  if ($LASTEXITCODE -ne 0) { throw "tar.exe packaging failed with exit code $LASTEXITCODE" }
} finally {
  Pop-Location
}

Write-Host "Created companion runtime archive: $archive"
Write-Host 'The archive excludes gsplat source and build logs. Extract it, then run install-gsplat-runtime.ps1.'
