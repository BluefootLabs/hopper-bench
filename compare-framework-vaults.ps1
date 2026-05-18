param(
    # Path to the main Hopper framework checkout. Defaults to the sibling
    # checkout name used by Hopper development workspaces.
    [string]$HopperRoot = "..\Hopper-Solana-Zero-copy-State-Framework",

    # Optional path to an extracted Quasar checkout. When supplied,
    # the `quasar` framework is added to the comparison. Pre-R2 this
    # was mandatory because the Pinocchio baseline was loaded from
    # Quasar's examples/pinocchio-vault; after R2 the Pinocchio
    # baseline is built in-tree from bench/pinocchio-vault so
    # -QuasarRoot is optional.
    [string]$QuasarRoot,

    # Optional path to an Anchor framework checkout. When supplied,
    # the `anchor` framework is added to the comparison matrix.
    [string]$AnchorRoot,

    [string]$OutDir = "bench\results\framework-vaults",

    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"

$benchRoot = $PSScriptRoot
$resolvedHopperRoot = (Resolve-Path -LiteralPath $HopperRoot).Path
if (-not (Test-Path -LiteralPath (Join-Path $resolvedHopperRoot "examples\hopper-parity-vault\Cargo.toml"))) {
    throw "Hopper root does not look valid: missing examples\hopper-parity-vault\Cargo.toml"
}

$resolvedOutDir = if ([System.IO.Path]::IsPathRooted($OutDir)) {
    $OutDir
} else {
    Join-Path $benchRoot $OutDir
}

$resolvedQuasarRoot = $null
if ($QuasarRoot) {
    $resolvedQuasarRoot = (Resolve-Path -LiteralPath $QuasarRoot).Path
    if (-not (Test-Path -LiteralPath (Join-Path $resolvedQuasarRoot "examples\vault\Cargo.toml"))) {
        throw "Quasar root does not look valid: missing examples\vault\Cargo.toml"
    }
}

$resolvedAnchorRoot = $null
if ($AnchorRoot) {
    $resolvedAnchorRoot = (Resolve-Path -LiteralPath $AnchorRoot).Path
}

New-Item -ItemType Directory -Force -Path $resolvedOutDir | Out-Null

function Invoke-CargoCapture {
    param(
        [Parameter(Mandatory = $true)]
        [string]$WorkingDirectory,

        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    Write-Host "==> cargo $($Arguments -join ' ')" -ForegroundColor Cyan

    $stdoutPath = [System.IO.Path]::GetTempFileName()
    $stderrPath = [System.IO.Path]::GetTempFileName()
    try {
        $startProcessArgs = @{
            FilePath = "cargo"
            ArgumentList = $Arguments
            WorkingDirectory = $WorkingDirectory
            NoNewWindow = $true
            Wait = $true
            PassThru = $true
            RedirectStandardOutput = $stdoutPath
            RedirectStandardError = $stderrPath
        }
        $process = Start-Process @startProcessArgs

        $stdout = if ((Get-Item -LiteralPath $stdoutPath).Length -gt 0) {
            Get-Content -LiteralPath $stdoutPath -Raw
        } else {
            ""
        }
        $stderr = if ((Get-Item -LiteralPath $stderrPath).Length -gt 0) {
            Get-Content -LiteralPath $stderrPath -Raw
        } else {
            ""
        }

        $text = ($stdout + $stderr).TrimEnd()

        if ($process.ExitCode -ne 0) {
            throw "cargo $($Arguments -join ' ') failed in $WorkingDirectory`n$text"
        }

        return $text
    }
    finally {
        Remove-Item -LiteralPath $stdoutPath, $stderrPath -Force -ErrorAction SilentlyContinue
    }
}

if (-not $NoBuild) {
    # In-tree baselines: always built.
    Invoke-CargoCapture -WorkingDirectory $resolvedHopperRoot -Arguments @("build-sbf", "--manifest-path", "examples/hopper-parity-vault/Cargo.toml") | Out-Null
    Invoke-CargoCapture -WorkingDirectory $benchRoot -Arguments @("build-sbf", "--manifest-path", "pinocchio-vault/Cargo.toml") | Out-Null

    # Optional external comparators.
    if ($resolvedQuasarRoot) {
        Invoke-CargoCapture -WorkingDirectory $resolvedQuasarRoot -Arguments @("build-sbf", "--manifest-path", "examples/vault/Cargo.toml") | Out-Null
    }
}

$runnerArgs = @(
    "run",
    "-p",
    "framework-vault-bench",
    "--",
    "--hopper-root",
    $resolvedHopperRoot,
    "--out-dir",
    $resolvedOutDir
)
if ($resolvedQuasarRoot) {
    $runnerArgs += @("--quasar-root", $resolvedQuasarRoot)
}
if ($resolvedAnchorRoot) {
    $runnerArgs += @("--anchor-root", $resolvedAnchorRoot)
}

$output = Invoke-CargoCapture -WorkingDirectory $benchRoot -Arguments $runnerArgs

Write-Host ""
Write-Host $output
