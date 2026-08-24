[CmdletBinding(SupportsShouldProcess = $true)]
param(
  [switch]$Force,
  [string]$CacheDirectory
)
$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $workspace 'engines\manifest.json'
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
$cuda = $manifest.engines | Where-Object { $_.name -eq 'COLMAP (CUDA)' } | Select-Object -First 1
if (-not $cuda) { throw 'Manifest has no "COLMAP (CUDA)" entry.' }
if (-not $CacheDirectory) { $CacheDirectory = Join-Path $workspace '.cache\engines' }
New-Item -ItemType Directory -Path $CacheDirectory -Force | Out-Null
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
& (Join-Path $PSScriptRoot 'setup-engines.ps1') -Force:$Force -CacheDirectory $CacheDirectory -IncludeOptional
if ($PSCmdlet.ShouldProcess('CUDA COLMAP install', 'verify engines')) {
  & (Join-Path $PSScriptRoot 'verify-engines.ps1')
}