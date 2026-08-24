[CmdletBinding()]
param(
  [switch]$Force,
  [string]$CacheDirectory,
  [switch]$IncludeOptional,
  [switch]$SkipHashCheck
)
$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $workspace 'engines\manifest.json'
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) {
  throw "Missing engine manifest: $manifestPath"
}
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if (-not $CacheDirectory) {
  $CacheDirectory = Join-Path $workspace '.cache\engines'
}
$CacheDirectory = [System.IO.Path]::GetFullPath($CacheDirectory)
New-Item -ItemType Directory -Path $CacheDirectory -Force | Out-Null

function Get-Sha256Hex([string]$LiteralPath) {
  $stream = [System.IO.File]::OpenRead($LiteralPath)
  try {
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
      return ([System.BitConverter]::ToString($sha256.ComputeHash($stream))).Replace('-', '')
    } finally {
      $sha256.Dispose()
    }
  } finally {
    $stream.Dispose()
  }
}

function Assert-Sha256([string]$Label, [string]$Value) {
  if ($Value -notmatch '^[0-9A-Fa-f]{64}$') {
    $lines = @(
      ('Invalid SHA-256 for ' + $Label + '. Expected exactly 64 hexadecimal characters.'),
      '',
      'The value is not a 64-char hex string.',
      'Fix: run Get-FileHash -Algorithm SHA256 <archivePath>, then paste the 64-char hash',
      'back into engines/manifest.json (archiveSha256 or sha256).',
      'Or pass -SkipHashCheck to bypass this check temporarily.'
    )
    $hint = ($lines -join "`n")
    throw $hint
  }
}

if (-not $SkipHashCheck) {
  foreach ($engine in $manifest.engines) {
    if ($engine.PSObject.Properties['localBuild'] -and $engine.localBuild) { continue }
    Assert-Sha256 "engine '$($engine.name)' archive" $engine.archiveSha256
  }
  foreach ($item in $manifest.requiredFiles) {
    Assert-Sha256 "required file '$($item.path)'" $item.sha256
  }
}
function Assert-WorkspaceDestination([string]$RelativePath) {
  $full = [System.IO.Path]::GetFullPath((Join-Path $workspace $RelativePath))
  $root = [System.IO.Path]::GetFullPath($workspace).TrimEnd('\') + '\'
  if (-not $full.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Engine destination escapes the workspace: $RelativePath"
  }
  return $full
}
function Test-RequiredFiles($Engine, [string]$Destination) {
  $prefix = ($Destination.Replace('\', '/').TrimEnd('/') + '/')
  $required = @($manifest.requiredFiles | Where-Object { $_.path.Replace('\', '/').StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase) })
  if ($required.Count -eq 0) { return $false }
  foreach ($item in $required) {
    $path = Join-Path $workspace $item.path
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $false }
    if ((Get-Sha256Hex $path) -ne $item.sha256) { return $false }
  }
  return $true
}
function Test-OptionalPresent([string]$Destination) {
  $prefix = ($Destination.Replace('\', '/').TrimEnd('/') + '/')
  $optionals = @($manifest.optionalFiles | Where-Object { $_.path.Replace('\', '/').StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase) })
  if ($optionals.Count -eq 0) { return $true }
  foreach ($item in $optionals) {
    $path = Join-Path $workspace $item.path
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $false }
  }
  return $true
}
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
foreach ($engine in $manifest.engines) {
  if ($engine.PSObject.Properties['localBuild'] -and $engine.localBuild) {
    Write-Host "Skipped (bundled local build): $($engine.name)"
    continue
  }
  if ($engine.PSObject.Properties['optional'] -and $engine.optional -and -not $IncludeOptional) {
    Write-Host "Skipped (optional, not requested): $($engine.name)"
    continue
  }
  $install = $engine.install
  if (-not $install) { throw "Engine '$($engine.name)' has no install configuration." }
  $isOptional = $engine.PSObject.Properties['optional'] -and $engine.optional
  $ready = if ($isOptional) { Test-OptionalPresent $install.destination } else { Test-RequiredFiles $engine $install.destination }
  if (-not $Force -and $ready) {
    Write-Host "Ready: $($engine.name)"
    continue
  }
  $archivePath = Join-Path $CacheDirectory $install.archiveName
  $download = $true
  if (Test-Path -LiteralPath $archivePath -PathType Leaf) {
    $download = (Get-Sha256Hex $archivePath) -ne $engine.archiveSha256
    if ($download) {
      Write-Warning "Cached archive hash changed; downloading a clean copy for $($engine.name)."
      Remove-Item -LiteralPath $archivePath -Force
    }
  }
  if ($download) {
    Write-Host "Downloading $($engine.name)..."
    Invoke-WebRequest -Uri $engine.sourceUrl -OutFile $archivePath -UseBasicParsing
  } else {
    Write-Host "Using cached archive: $($install.archiveName)"
  }
  $archiveHash = Get-Sha256Hex $archivePath
  if ($archiveHash -ne $engine.archiveSha256) {
    throw "Archive hash mismatch for $($engine.name). Expected $($engine.archiveSha256), got $archiveHash. The upstream asset may have changed; update manifest.json only after reviewing the new release."
  }
  $temporary = Join-Path $workspace ('.tmp\engine-setup-' + [guid]::NewGuid().ToString('N'))
  try {
    New-Item -ItemType Directory -Path $temporary -Force | Out-Null
    Expand-Archive -LiteralPath $archivePath -DestinationPath $temporary -Force
    $anchors = @(Get-ChildItem -LiteralPath $temporary -Recurse -File | Where-Object { $_.Name -eq $install.anchorFile })
    if ($anchors.Count -ne 1) {
      throw "Expected one '$($install.anchorFile)' in $($install.archiveName), found $($anchors.Count)."
    }
    $sourceRoot = $anchors[0].Directory
    for ($level = 0; $level -lt [int]$install.rootFromAnchorParent; $level++) {
      $sourceRoot = $sourceRoot.Parent
      if (-not $sourceRoot) { throw "Invalid extraction root for $($engine.name)." }
    }
    $destination = Assert-WorkspaceDestination $install.destination
    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    Get-ChildItem -LiteralPath $destination -Force |
      Where-Object { $_.Name -ne 'README.md' } |
      Remove-Item -Recurse -Force
    Get-ChildItem -LiteralPath $sourceRoot.FullName -Force |
      Where-Object { $_.Name -ne 'README.md' } |
      ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $destination -Recurse -Force }
    Write-Host "Installed: $($engine.name) -> $($install.destination)"
  } finally {
    if (Test-Path -LiteralPath $temporary) {
      Remove-Item -LiteralPath $temporary -Recurse -Force
    }
  }
}
& (Join-Path $PSScriptRoot 'verify-engines.ps1')
