param(
    [Parameter(Mandatory = $true)]
    [string]$RequestPath,
    [int]$WarmupStep = 1000,
    [int]$PreScreenStep = 5000,
    [string]$OutputRoot,
    [string]$GsplatRoot,
    [double]$MinPsnrGain = 0.05,
    [double]$MinSsimGain = 0.001,
    [double]$MaxSplatIncrease = 0.05,
    [double]$MaxVramIncrease = 0.10
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

function Read-RunMetrics([string]$Directory, [string]$LogPath, [bool]$CheckpointOnly) {
    $metricsPath = Join-Path $Directory 'logs\quality\validation-metrics.json'
    $metrics = if (Test-Path -LiteralPath $metricsPath) { Read-Json $metricsPath } else { $null }
    $events = @(
        foreach ($line in Get-Content -LiteralPath $LogPath) {
            try { $line | ConvertFrom-Json } catch { }
        }
    )
    $event = if ($CheckpointOnly) {
        @($events | Where-Object { $_.event -eq 'checkpoint' -and $_.mode -eq 'stopped' } | Select-Object -Last 1)
    } else {
        @($events | Where-Object { $_.event -eq 'export' } | Select-Object -Last 1)
    }
    $completed = @($events | Where-Object { $_.event -eq 'completed' } | Select-Object -Last 1)
    $peak = @($events | Where-Object { $_.event -eq 'metric' -and $_.name -eq 'peakVramMb' } | Select-Object -Last 1)
    $eventObject = if (@($event).Count -gt 0) { @($event)[0] } else { $null }
    $eventSplats = if ($null -ne $eventObject -and $null -ne $eventObject.PSObject.Properties['splats']) { [int]$eventObject.splats } else { $null }
    $eventLogicalSplats = if ($null -ne $eventObject -and $null -ne $eventObject.PSObject.Properties['logicalSplats']) { [int]$eventObject.logicalSplats } else { $null }
    $eventPeakVram = if ($null -ne $eventObject -and $null -ne $eventObject.PSObject.Properties['peakVramMb']) { [int]$eventObject.peakVramMb } else { $null }
    [pscustomobject]@{
        psnr = if ($null -ne $metrics) { [double]$metrics.psnr } else { $null }
        ssim = if ($null -ne $metrics) { [double]$metrics.ssim } else { $null }
        l1 = if ($null -ne $metrics) { [double]$metrics.l1 } else { $null }
        elapsedSeconds = if (@($completed).Count -gt 0) { [math]::Round(([double]@($completed)[0].elapsedMs / 1000), 1) } else { $null }
        splats = $eventSplats
        logicalSplats = $eventLogicalSplats
        peakVramMb = if ($null -ne $eventPeakVram) { $eventPeakVram } elseif (@($peak).Count -gt 0) { [int]@($peak)[0].value } else { $null }
        outputPly = Join-Path $Directory 'final.ply'
    }
}

$scriptRoot = if ($PSScriptRoot) { $PSScriptRoot -replace '^\\\\\?\\', '' } else { $null }
$GsplatRoot = if ($GsplatRoot) { $GsplatRoot -replace '^\\\\\?\\', '' } else { $null }
$repoRoot = if ($scriptRoot) { Split-Path -Parent $scriptRoot } else { $null }
if (-not $GsplatRoot -and $scriptRoot) {
    # Tauri development resources live under src-tauri/target/<profile>/scripts,
    # while the ignored gsplat runtime stays at the workspace engines/ folder.
    $cursor = Get-Item -LiteralPath $scriptRoot -ErrorAction SilentlyContinue
    while ($null -ne $cursor) {
        $candidate = Join-Path $cursor.FullName 'engines\gsplat'
        if (Test-Path -LiteralPath (Join-Path $candidate 'python\Scripts\python.exe')) {
            $GsplatRoot = $candidate
            break
        }
        $cursor = $cursor.Parent
    }
}
if (-not $GsplatRoot -and $repoRoot) {
    $GsplatRoot = Join-Path $repoRoot 'engines\gsplat'
}
$python = if ($GsplatRoot) { Join-Path $GsplatRoot 'python\Scripts\python.exe' } else { $null }
$adapter = if ($GsplatRoot) { Join-Path $GsplatRoot 'adapter\train_adapter.py' } else { $null }
if (-not (Test-Path -LiteralPath $python)) { throw "bundled Python not found: $python" }
if (-not (Test-Path -LiteralPath $adapter)) { throw "gsplat adapter not found: $adapter" }

$base = Read-Json $RequestPath
$total = [int]$base.maxSteps
$refineStart = [math]::Max(500, [int]($total * 0.1))
if ($WarmupStep -lt 1 -or $WarmupStep -ge $refineStart) { throw "WarmupStep must be before densification starts" }
if ($PreScreenStep -le $WarmupStep -or $PreScreenStep -ge $total) { throw "PreScreenStep must be after warmup and before maxSteps" }
if (-not $OutputRoot) { $OutputRoot = Join-Path (Split-Path -Parent $RequestPath) 'auto-select' }
New-Item -ItemType Directory -Force -Path $OutputRoot | Out-Null
$sharedCheckpoint = Join-Path $OutputRoot 'shared-warmup.pt'

$warmup = $base | ConvertTo-Json -Depth 20 | ConvertFrom-Json
$warmupDir = Join-Path $OutputRoot 'warmup'
Set-RequestProperty $warmup 'resultDir' $warmupDir
Set-RequestProperty $warmup 'outputPly' (Join-Path $warmupDir 'unused.ply')
Set-RequestProperty $warmup 'diagnosticsDir' (Join-Path $warmupDir 'logs')
Set-RequestProperty $warmup 'strategy' 'mcmc'
Set-RequestProperty $warmup 'multiViewDensificationGate' $false
Set-RequestProperty $warmup 'multiViewMinSupport' 2
Set-RequestProperty $warmup 'checkpointStep' $WarmupStep
Set-RequestProperty $warmup 'checkpointPath' $sharedCheckpoint
Set-RequestProperty $warmup 'stopAfterCheckpoint' $true
Set-RequestProperty $warmup 'resumeCheckpoint' $null
$warmupConfig = Join-Path $OutputRoot 'warmup.json'
Write-Json $warmup $warmupConfig
Invoke-Adapter $warmupConfig (Join-Path $OutputRoot 'warmup.log')
if (-not (Test-Path -LiteralPath $sharedCheckpoint)) { throw "warmup checkpoint was not created" }

$candidates = @(
    foreach ($policy in @('mcmc', 'gate3')) {
        $branchDir = Join-Path $OutputRoot "prescreen-$policy"
        $branch = $base | ConvertTo-Json -Depth 20 | ConvertFrom-Json
        Set-RequestProperty $branch 'resultDir' $branchDir
        Set-RequestProperty $branch 'outputPly' (Join-Path $branchDir 'unused.ply')
        Set-RequestProperty $branch 'diagnosticsDir' (Join-Path $branchDir 'logs')
        Set-RequestProperty $branch 'strategy' 'mcmc'
        Set-RequestProperty $branch 'multiViewDensificationGate' ($policy -eq 'gate3')
        Set-RequestProperty $branch 'multiViewMinSupport' 3
        Set-RequestProperty $branch 'checkpointStep' $PreScreenStep
        Set-RequestProperty $branch 'checkpointPath' (Join-Path $branchDir 'prescreen.pt')
        Set-RequestProperty $branch 'stopAfterCheckpoint' $true
        Set-RequestProperty $branch 'resumeCheckpoint' $sharedCheckpoint
        $configPath = Join-Path $branchDir 'request.json'
        $logPath = Join-Path $branchDir 'adapter.log'
        Write-Json $branch $configPath
        Invoke-Adapter $configPath $logPath
        $metrics = Read-RunMetrics $branchDir $logPath $true
        $metrics | Add-Member -NotePropertyName policy -NotePropertyValue $policy
        $metrics | Add-Member -NotePropertyName checkpoint -NotePropertyValue (Join-Path $branchDir 'prescreen.pt')
        $metrics
    }
)

$mcmc = $candidates | Where-Object policy -eq 'mcmc' | Select-Object -First 1
$gate3 = $candidates | Where-Object policy -eq 'gate3' | Select-Object -First 1
$gatePasses = $gate3.psnr -ge ($mcmc.psnr + $MinPsnrGain) -and
    $gate3.ssim -ge ($mcmc.ssim + $MinSsimGain) -and
    $gate3.l1 -le $mcmc.l1 -and
    $gate3.logicalSplats -le ($mcmc.logicalSplats * (1 + $MaxSplatIncrease)) -and
    $gate3.peakVramMb -le ($mcmc.peakVramMb * (1 + $MaxVramIncrease))
$selected = if ($gatePasses) { $gate3 } else { $mcmc }

$finalDir = Join-Path $OutputRoot 'selected'
$final = $base | ConvertTo-Json -Depth 20 | ConvertFrom-Json
Set-RequestProperty $final 'resultDir' $finalDir
Set-RequestProperty $final 'outputPly' (Join-Path $finalDir 'final.ply')
Set-RequestProperty $final 'diagnosticsDir' (Join-Path $finalDir 'logs')
Set-RequestProperty $final 'strategy' 'mcmc'
Set-RequestProperty $final 'multiViewDensificationGate' ($selected.policy -eq 'gate3')
Set-RequestProperty $final 'multiViewMinSupport' 3
Set-RequestProperty $final 'checkpointStep' 0
Set-RequestProperty $final 'checkpointPath' $null
Set-RequestProperty $final 'stopAfterCheckpoint' $false
Set-RequestProperty $final 'resumeCheckpoint' $selected.checkpoint
$finalConfig = Join-Path $finalDir 'request.json'
$finalLog = Join-Path $finalDir 'adapter.log'
Write-Json $final $finalConfig
Invoke-Adapter $finalConfig $finalLog
$finalMetrics = Read-RunMetrics $finalDir $finalLog $false

$summary = [ordered]@{
    schemaVersion = 1
    requestPath = $RequestPath
    warmupStep = $WarmupStep
    preScreenStep = $PreScreenStep
    thresholds = [ordered]@{ minPsnrGain = $MinPsnrGain; minSsimGain = $MinSsimGain; maxSplatIncrease = $MaxSplatIncrease; maxVramIncrease = $MaxVramIncrease }
    preScreen = $candidates
    selectedPolicy = $selected.policy
    gate3Passed = $gatePasses
    final = $finalMetrics
}
Write-Json ([pscustomobject]$summary) (Join-Path $OutputRoot 'auto-select.json')
$summary | ConvertTo-Json -Depth 20
