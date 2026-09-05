param(
    [Parameter(Mandatory = $true)]
    [string]$RequestPath,
    [int]$CheckpointStep = 1000,
    [string]$OutputRoot,
    [string]$Policies = 'mcmc,gate2,gate3'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Set-RequestProperty($Object, [string]$Name, $Value) {
    if ($null -eq $Object.PSObject.Properties[$Name]) {
        $Object | Add-Member -NotePropertyName $Name -NotePropertyValue $Value
    } else {
        $Object.$Name = $Value
    }
}

function Read-Json([string]$Path) {
    Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json
}

function Write-Json($Object, [string]$Path) {
    $parent = Split-Path -Parent $Path
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, ($Object | ConvertTo-Json -Depth 20), $utf8NoBom)
}

function Invoke-Adapter([string]$ConfigPath, [string]$LogPath) {
    & $python $adapter --config $ConfigPath *> $LogPath
    if ($LASTEXITCODE -ne 0) {
        throw "gsplat adapter failed for $ConfigPath (exit $LASTEXITCODE)"
    }
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$python = Join-Path $repoRoot 'engines\gsplat\python\Scripts\python.exe'
$adapter = Join-Path $repoRoot 'engines\gsplat\adapter\train_adapter.py'
if (-not (Test-Path -LiteralPath $python)) { throw "bundled Python not found: $python" }
if (-not (Test-Path -LiteralPath $adapter)) { throw "gsplat adapter not found: $adapter" }

$base = Read-Json $RequestPath
$policyList = @($Policies -split ',' | ForEach-Object { $_.Trim().ToLowerInvariant() } | Where-Object { $_ })
if (@($policyList | Where-Object { $_ -notin @('mcmc', 'gate2', 'gate3') }).Count -gt 0) {
    throw "Policies must be a comma-separated list of mcmc,gate2,gate3"
}
$total = [int]$base.maxSteps
$refineStart = [math]::Max(500, [int]($total * 0.1))
if ($CheckpointStep -lt 1 -or $CheckpointStep -ge $refineStart) {
    throw "CheckpointStep must be between 1 and $($refineStart - 1) for maxSteps=$total"
}
if (-not $OutputRoot) {
    $OutputRoot = Join-Path (Split-Path -Parent $RequestPath) 'policy-sweep'
}
New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null
$checkpoint = Join-Path $OutputRoot 'shared-warmup.pt'

$warmup = $base | ConvertTo-Json -Depth 20 | ConvertFrom-Json
$warmupDir = Join-Path $OutputRoot 'warmup'
Set-RequestProperty $warmup 'resultDir' $warmupDir
Set-RequestProperty $warmup 'outputPly' (Join-Path $warmupDir 'unused.ply')
Set-RequestProperty $warmup 'diagnosticsDir' (Join-Path $warmupDir 'logs')
Set-RequestProperty $warmup 'multiViewDensificationGate' $false
Set-RequestProperty $warmup 'multiViewMinSupport' 2
Set-RequestProperty $warmup 'checkpointStep' $CheckpointStep
Set-RequestProperty $warmup 'checkpointPath' $checkpoint
Set-RequestProperty $warmup 'stopAfterCheckpoint' $true
Set-RequestProperty $warmup 'resumeCheckpoint' $null
$warmupConfig = Join-Path $OutputRoot 'warmup.json'
Write-Json $warmup $warmupConfig
Invoke-Adapter $warmupConfig (Join-Path $OutputRoot 'warmup.log')
if (-not (Test-Path -LiteralPath $checkpoint)) { throw "checkpoint was not created: $checkpoint" }

$results = @(
    foreach ($policy in $policyList) {
        $branch = $base | ConvertTo-Json -Depth 20 | ConvertFrom-Json
        $branchDir = Join-Path $OutputRoot $policy
        $gate = $policy -ne 'mcmc'
        $support = if ($policy -eq 'gate3') { 3 } else { 2 }
        Set-RequestProperty $branch 'resultDir' $branchDir
        Set-RequestProperty $branch 'outputPly' (Join-Path $branchDir 'final.ply')
        Set-RequestProperty $branch 'diagnosticsDir' (Join-Path $branchDir 'logs')
        Set-RequestProperty $branch 'strategy' 'mcmc'
        Set-RequestProperty $branch 'multiViewDensificationGate' $gate
        Set-RequestProperty $branch 'multiViewMinSupport' $support
        Set-RequestProperty $branch 'checkpointStep' 0
        Set-RequestProperty $branch 'checkpointPath' $null
        Set-RequestProperty $branch 'stopAfterCheckpoint' $false
        Set-RequestProperty $branch 'resumeCheckpoint' $checkpoint
        $branchConfig = Join-Path $branchDir 'request.json'
        Write-Json $branch $branchConfig
        $branchLog = Join-Path $branchDir 'adapter.log'
        Invoke-Adapter $branchConfig $branchLog
        $logText = Get-Content -Raw -LiteralPath $branchLog
        $completedMatch = [regex]::Match($logText, '"event": "completed", "elapsedMs": ([0-9]+)')
        $exportMatch = [regex]::Match($logText, '"event": "export", "path": .*?, "splats": ([0-9]+), "logicalSplats": ([0-9]+)')
        $vramMatch = [regex]::Match($logText, '"name": "peakVramMb", "value": ([0-9]+)')
        $metricsPath = Join-Path $branchDir 'logs\quality\validation-metrics.json'
        $metrics = if (Test-Path -LiteralPath $metricsPath) { Read-Json $metricsPath } else { $null }
        $plyPath = Join-Path $branchDir 'final.ply'
        [pscustomobject]@{
            policy = $policy
            resultDir = $branchDir
            psnr = if ($null -ne $metrics) { $metrics.psnr } else { $null }
            ssim = if ($null -ne $metrics) { $metrics.ssim } else { $null }
            l1 = if ($null -ne $metrics) { $metrics.l1 } else { $null }
            elapsedSeconds = if ($completedMatch.Success) { [math]::Round(([double]$completedMatch.Groups[1].Value / 1000), 1) } else { $null }
            splats = if ($exportMatch.Success) { [int]$exportMatch.Groups[1].Value } else { $null }
            logicalSplats = if ($exportMatch.Success) { [int]$exportMatch.Groups[2].Value } else { $null }
            peakVramMb = if ($vramMatch.Success) { [int]$vramMatch.Groups[1].Value } else { $null }
            sizeMiB = if (Test-Path -LiteralPath $plyPath) { [math]::Round(((Get-Item -LiteralPath $plyPath).Length / 1MB), 1) } else { $null }
            outputPly = $plyPath
        }
    }
)

$summary = [ordered]@{
    schemaVersion = 1
    checkpointStep = $CheckpointStep
    checkpoint = $checkpoint
    sourceRequest = $RequestPath
    candidates = $results
}
Write-Json ([pscustomobject]$summary) (Join-Path $OutputRoot 'policy-sweep.json')
$results | Format-Table -AutoSize
