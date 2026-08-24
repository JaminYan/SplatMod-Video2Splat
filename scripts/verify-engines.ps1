$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $workspace 'engines\manifest.json'
if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw "Missing engine manifest: $manifestPath" }
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json

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
      'The value is not a 64-char hex string. Recompute with',
      'Get-FileHash -Algorithm SHA256 <filePath> and paste it back into engines/manifest.json.'
    )
    $hint = ($lines -join "`n")
    throw $hint
  }
}

foreach ($engine in $manifest.engines) {
  if ($engine.PSObject.Properties['localBuild'] -and $engine.localBuild) { continue }
  Assert-Sha256 "engine '$($engine.name)' archive" $engine.archiveSha256
}
foreach ($item in $manifest.requiredFiles) {
  Assert-Sha256 "required file '$($item.path)'" $item.sha256
}
foreach ($item in @($manifest.optionalFiles)) {
  Assert-Sha256 "optional file '$($item.path)'" $item.sha256
}

foreach ($item in $manifest.requiredFiles) {
  $path = Join-Path $workspace $item.path
  if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing engine file: $($item.path). Run 'npm run setup:engines' first." }
  $actual = Get-Sha256Hex $path
  if ($actual -ne $item.sha256) { throw "Hash mismatch for $($item.path): $actual" }
}
# CPU/no-CUDA COLMAP must remain free of CUDA runtime artefacts.
$cudaFiles = Get-ChildItem -LiteralPath (Join-Path $workspace 'engines\colmap') -Recurse -File | Where-Object { $_.Name -match '(?i)cudart|cublas|cudnn|cuda\.dll' }
if ($cudaFiles) { throw "CUDA runtime found in CPU COLMAP package: $($cudaFiles.FullName -join ', ')" }
$colmap = Join-Path $workspace 'engines\colmap\bin\colmap.exe'
$savedPreference = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
$help = & $colmap feature_extractor -h 2>&1 | Out-String
$colmapExit = $LASTEXITCODE
if ($colmapExit -ne 0 -or $help -notmatch '(?i)without CUDA') { throw 'Bundled COLMAP did not explicitly report without CUDA.' }
$brush = Join-Path $workspace 'engines\brush\brush_app.exe'
$brushHelp = & $brush --help 2>&1 | Out-String
$brushExit = $LASTEXITCODE
$ErrorActionPreference = $savedPreference
if ($brushExit -ne 0) { throw "Bundled Brush help failed with exit code $brushExit" }
foreach ($flag in '--total-steps','--max-resolution','--export-path','--export-name') {
  if ($brushHelp -notmatch [regex]::Escape($flag)) { throw "Bundled Brush is missing $flag" }
}
# Optional: CUDA COLMAP, only verified when the user has downloaded it.
$cudaColmapDir = Join-Path $workspace 'engines\colmap-cuda'
if (Test-Path -LiteralPath $cudaColmapDir) {
  $cudaColmap = Join-Path $cudaColmapDir 'bin\colmap.exe'
  if (-not (Test-Path -LiteralPath $cudaColmap -PathType Leaf)) {
    throw "CUDA COLMAP directory is present but $cudaColmap is missing. Delete '$cudaColmapDir' or re-run 'npm run setup:engines -- -IncludeOptional'."
  }
  $savedPreference = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  $cudaHelp = & $cudaColmap feature_extractor -h 2>&1 | Out-String
  $cudaExit = $LASTEXITCODE
  $ErrorActionPreference = $savedPreference
  if ($cudaExit -ne 0) { throw "Bundled CUDA COLMAP help failed with exit code $cudaExit" }
  if ($cudaHelp -notmatch '(?i)with CUDA|cuda support: yes') {
    throw 'Bundled CUDA COLMAP did not advertise GPU/CUDA support.'
  }
  foreach ($flag in '--FeatureExtraction.use_gpu','--FeatureExtraction.gpu_index') {
    if ($cudaHelp -notmatch [regex]::Escape($flag)) { throw "Bundled CUDA COLMAP is missing $flag" }
  }
  $savedPreference = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  $cudaMatcherHelp = & $cudaColmap sequential_matcher -h 2>&1 | Out-String
  if ($LASTEXITCODE -ne 0) { throw 'Bundled CUDA COLMAP sequential_matcher help failed.' }
  $ErrorActionPreference = $savedPreference
  foreach ($flag in '--FeatureMatching.use_gpu','--FeatureMatching.gpu_index') {
    if ($cudaMatcherHelp -notmatch [regex]::Escape($flag)) { throw "Bundled CUDA COLMAP matcher is missing $flag" }
  }
  $ErrorActionPreference = 'Continue'
  $cudaMapperHelp = & $cudaColmap mapper -h 2>&1 | Out-String
  if ($LASTEXITCODE -ne 0) { throw 'Bundled CUDA COLMAP mapper help failed.' }
  $ErrorActionPreference = $savedPreference
  if ($cudaMapperHelp -notmatch [regex]::Escape('--Mapper.ba_local_backend')) {
    throw 'Bundled CUDA COLMAP mapper is missing the bundle-adjustment backend option.'
  }
  foreach ($item in @($manifest.optionalFiles | Where-Object { $_.path.Replace('\', '/').StartsWith('engines/colmap-cuda/', [System.StringComparison]::OrdinalIgnoreCase) })) {
    $path = Join-Path $workspace $item.path
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Missing optional CUDA engine file: $($item.path)" }
    $actual = Get-Sha256Hex $path
    if ($actual -ne $item.sha256) { throw "Hash mismatch for optional CUDA file $($item.path): $actual" }
  }
  Write-Host "Verified optional CUDA COLMAP at $cudaColmap"
}
 $caspar = Join-Path $workspace 'engines\colmap-caspar\bin\colmap.exe'
if (Test-Path -LiteralPath $caspar) {
  $savedPreference = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  $casparHelp = & $caspar bundle_adjuster -h 2>&1 | Out-String
  $casparExit = $LASTEXITCODE
  $ErrorActionPreference = $savedPreference
  if ($casparExit -ne 0 -or $casparHelp -notmatch 'BundleAdjustmentCaspar\.solver_iter_max') {
    throw 'Bundled CASPAR COLMAP is missing its bundle-adjuster options.'
  }
  foreach ($item in @($manifest.optionalFiles | Where-Object { $_.path -eq 'engines/colmap-caspar/bin/colmap.exe' })) {
    if ((Get-Sha256Hex (Join-Path $workspace $item.path)) -ne $item.sha256) { throw 'CASPAR COLMAP hash mismatch.' }
  }
  Write-Host "Verified bundled CASPAR COLMAP at $caspar"
}
Write-Host "Verified $($manifest.requiredFiles.Count) locked engine files; COLMAP CPU/no-CUDA and Brush CLI are valid."
