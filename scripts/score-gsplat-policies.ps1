param(
    [string]$Root = 'A:\tmp\Splatcam',
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Read-JsonFile([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    return Get-Content -Raw -LiteralPath $Path | ConvertFrom-Json
}

function Normalize([double]$Value, [double]$Minimum, [double]$Maximum) {
    if ($Maximum -le $Minimum) { return 0.0 }
    return [math]::Max(0.0, [math]::Min(1.0, ($Value - $Minimum) / ($Maximum - $Minimum)))
}

$entries = @(
    foreach ($directory in Get-ChildItem -LiteralPath $Root -Directory) {
        $project = Read-JsonFile (Join-Path $directory.FullName 'project.json')
        if ($null -eq $project -or $project.status -ne 'completed') { continue }
        $request = Read-JsonFile (Join-Path $directory.FullName 'work\gsplat\request.json')
        $validation = Read-JsonFile (Join-Path $directory.FullName 'logs\quality\validation-metrics.json')
        $diagnostics = Read-JsonFile (Join-Path $directory.FullName 'logs\floater-diagnostics.json')
        if ($null -eq $validation) { continue }

        $peakVram = $null
        $logPath = Join-Path $directory.FullName 'logs\gsplat.log'
        if (Test-Path -LiteralPath $logPath) {
            $match = [regex]::Match((Get-Content -Raw -LiteralPath $logPath), '"name":\s*"peakVramMb",\s*"value":\s*([0-9.]+)')
            if ($match.Success) { $peakVram = [double]$match.Groups[1].Value }
        }

        $strategy = 'MCMC'
        if ($project.gsplatDensificationStrategy -eq 'absgrad') { $strategy = 'AbsGS' }
        $gate = $false
        if ($null -ne $request -and $null -ne $request.PSObject.Properties['multiViewDensificationGate']) {
            $gate = [bool]$request.PSObject.Properties['multiViewDensificationGate'].Value
        }
        $photometricMode = $null
        if ($null -ne $project.PSObject.Properties['photometricMode']) {
            $photometricMode = $project.PSObject.Properties['photometricMode'].Value
        }
        $policy = "gsplat $strategy"
        if ($project.trainingBackend -eq 'brush') {
            $policy = "Brush $($project.brushTrainingPreset.ToUpperInvariant())"
        } elseif ($photometricMode -eq 'wdr10k') {
            $policy = "gsplat $strategy + WD-R 10k"
        } elseif ($photometricMode -eq 'wdr') {
            $policy = "gsplat $strategy + WD-R 15k"
        } elseif ($gate) {
            $policy = "gsplat $strategy + multiview-gate"
        }
        $highResidual = 0.0
        if ($null -ne $diagnostics -and $null -ne $diagnostics.PSObject.Properties['reprojectionConsistency']) {
            $reprojection = $diagnostics.PSObject.Properties['reprojectionConsistency'].Value
            if ($null -ne $reprojection -and $null -ne $reprojection.PSObject.Properties['highResidualFraction']) {
                $highResidual = [double]$reprojection.PSObject.Properties['highResidualFraction'].Value
            }
        }

        [pscustomobject]@{
            source = Split-Path $project.sourcePath -Leaf
            run = $directory.Name
            policy = $policy
            psnr = [double]$validation.psnr
            ssim = [double]$validation.ssim
            l1 = [double]$validation.l1
            highResidual = $highResidual
            durationSeconds = [math]::Round(([double]$project.durationMs / 1000), 1)
            splats = [int]$project.output.splatCount
            sizeMiB = [math]::Round(([double]$project.output.fileSize / 1MB), 1)
            peakVramMb = $peakVram
        }
    }
)

if ($entries.Count -eq 0) { throw "No completed runs with validation metrics found under $Root" }

$reports = @(
    foreach ($group in ($entries | Group-Object source)) {
        $items = @($group.Group)
        $psnrValues = @($items | ForEach-Object psnr)
        $ssimValues = @($items | ForEach-Object ssim)
        $l1Values = @($items | ForEach-Object l1)
        $timeValues = @($items | ForEach-Object durationSeconds)
        $sizeValues = @($items | ForEach-Object sizeMiB)
        $vramValues = @($items | Where-Object { $null -ne $_.peakVramMb } | ForEach-Object peakVramMb)
        $residualValues = @($items | ForEach-Object highResidual)
        $scored = @(
            @(
                foreach ($item in $items) {
                $quality = 0.45 * (Normalize $item.psnr ($psnrValues | Measure-Object -Minimum).Minimum ($psnrValues | Measure-Object -Maximum).Maximum)
                $quality += 0.35 * (Normalize $item.ssim ($ssimValues | Measure-Object -Minimum).Minimum ($ssimValues | Measure-Object -Maximum).Maximum)
                $quality += 0.20 * (1.0 - (Normalize $item.l1 ($l1Values | Measure-Object -Minimum).Minimum ($l1Values | Measure-Object -Maximum).Maximum))
                $cost = 0.15 * (Normalize $item.durationSeconds ($timeValues | Measure-Object -Minimum).Minimum ($timeValues | Measure-Object -Maximum).Maximum)
                $cost += 0.05 * (Normalize $item.sizeMiB ($sizeValues | Measure-Object -Minimum).Minimum ($sizeValues | Measure-Object -Maximum).Maximum)
                if ($vramValues.Count -gt 0 -and $null -ne $item.peakVramMb) { $cost += 0.05 * (Normalize $item.peakVramMb ($vramValues | Measure-Object -Minimum).Minimum ($vramValues | Measure-Object -Maximum).Maximum) }
                $cost += 0.10 * (Normalize $item.highResidual ($residualValues | Measure-Object -Minimum).Minimum ($residualValues | Measure-Object -Maximum).Maximum)
                [pscustomobject]@{
                    run = $item.run
                    policy = $item.policy
                    score = [math]::Round(($quality - $cost), 5)
                    psnr = $item.psnr
                    ssim = $item.ssim
                    l1 = $item.l1
                    durationSeconds = $item.durationSeconds
                    splats = $item.splats
                    sizeMiB = $item.sizeMiB
                    peakVramMb = $item.peakVramMb
                    highResidual = $item.highResidual
                }
                }
            ) | Sort-Object score -Descending
        )
        [pscustomobject]@{
            source = $group.Name
            candidateCount = $scored.Count
            recommendation = $scored[0].policy
            candidates = $scored
        }
    }
)

$result = [ordered]@{
    schemaVersion = 1
    generatedAt = (Get-Date).ToUniversalTime().ToString('o')
    root = $Root
    scoring = 'quality 0.45 PSNR + 0.35 SSIM + 0.20 inverse L1 - time/size/VRAM/residual penalties'
    reports = $reports
}
$json = $result | ConvertTo-Json -Depth 8
if ($OutputPath) {
    Set-Content -LiteralPath $OutputPath -Value $json -Encoding UTF8
} else {
    $json
}
