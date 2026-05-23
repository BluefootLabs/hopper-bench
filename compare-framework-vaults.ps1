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

    # Deploy every built framework artifact to this cluster before running
    # the benchmark report. The report then uses the deployed program IDs.
    [switch]$DeployDevnet,

    [string]$RpcUrl = "https://api.devnet.solana.com",

    [string]$Keypair,

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

function Invoke-ExternalCapture {
    param(
        [Parameter(Mandatory = $true)]
        [string]$FilePath,

        [Parameter(Mandatory = $true)]
        [string[]]$Arguments,

        [string]$WorkingDirectory = $benchRoot
    )

    Write-Host "==> $FilePath $($Arguments -join ' ')" -ForegroundColor Cyan

    $stdoutPath = [System.IO.Path]::GetTempFileName()
    $stderrPath = [System.IO.Path]::GetTempFileName()
    try {
        $startProcessArgs = @{
            FilePath = $FilePath
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
            throw "$FilePath $($Arguments -join ' ') failed in $WorkingDirectory`n$text"
        }

        return $text
    }
    finally {
        Remove-Item -LiteralPath $stdoutPath, $stderrPath -Force -ErrorAction SilentlyContinue
    }
}

function Select-RegexValue {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Text,

        [Parameter(Mandatory = $true)]
        [string]$Pattern
    )

    $match = [regex]::Match($Text, $Pattern, [System.Text.RegularExpressions.RegexOptions]::Multiline)
    if ($match.Success) {
        return $match.Groups[1].Value.Trim()
    }
    return $null
}

function Deploy-FrameworkProgram {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Framework,

        [Parameter(Mandatory = $true)]
        [string]$BinaryPath,

        [Parameter(Mandatory = $true)]
        [string]$DeployDir,

        [string]$ProgramKeypair
    )

    if (-not (Test-Path -LiteralPath $BinaryPath)) {
        throw "missing $Framework deploy artifact: $BinaryPath"
    }

    if (-not $ProgramKeypair) {
        $ProgramKeypair = New-ProgramKeypair -Framework $Framework -DeployDir $DeployDir
    }

    $programId = (Invoke-ExternalCapture -FilePath "solana-keygen" -Arguments @(
        "pubkey",
        $ProgramKeypair
    )).Trim()

    $deployText = Invoke-ExternalCapture -FilePath "solana" -Arguments @(
        "--keypair",
        $Keypair,
        "--url",
        $RpcUrl,
        "program",
        "deploy",
        $BinaryPath,
        "--program-id",
        $ProgramKeypair
    )

    $deployedProgramId = Select-RegexValue -Text $deployText -Pattern 'Program Id:\s*(\S+)'
    if (-not $deployedProgramId) {
        $deployedProgramId = $programId
    }
    $signature = Select-RegexValue -Text $deployText -Pattern 'Signature:\s*(\S+)'

    $showText = Invoke-ExternalCapture -FilePath "solana" -Arguments @(
        "--url",
        $RpcUrl,
        "program",
        "show",
        $deployedProgramId
    )

    [ordered]@{
        framework = $Framework
        programId = $deployedProgramId
        programKeypair = $ProgramKeypair
        binaryPath = $BinaryPath
        binarySizeBytes = (Get-Item -LiteralPath $BinaryPath).Length
        deploySignature = $signature
        programDataAddress = Select-RegexValue -Text $showText -Pattern 'ProgramData Address:\s*(\S+)'
        authority = Select-RegexValue -Text $showText -Pattern 'Authority:\s*(\S+)'
        rpcUrl = $RpcUrl
        deployedAt = [DateTime]::UtcNow.ToString("o")
    }
}

function New-ProgramKeypair {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Framework,

        [Parameter(Mandatory = $true)]
        [string]$DeployDir
    )

    $programKeypair = Join-Path $DeployDir "$Framework-program-keypair.json"
    Invoke-ExternalCapture -FilePath "solana-keygen" -Arguments @(
        "new",
        "--no-bip39-passphrase",
        "--force",
        "--outfile",
        $programKeypair
    ) | Out-Null
    return $programKeypair
}

function New-AnchorDevnetBuildRoot {
    param(
        [Parameter(Mandatory = $true)]
        [string]$SourceRoot,

        [Parameter(Mandatory = $true)]
        [string]$DeployDir,

        [Parameter(Mandatory = $true)]
        [string]$ProgramId
    )

    $buildRoot = Join-Path $DeployDir "anchor-vault-devnet-build"
    if (Test-Path -LiteralPath $buildRoot) {
        Remove-Item -LiteralPath $buildRoot -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $buildRoot | Out-Null

    Copy-Item -LiteralPath (Join-Path $SourceRoot "Cargo.toml") -Destination (Join-Path $buildRoot "Cargo.toml")
    Copy-Item -LiteralPath (Join-Path $SourceRoot "Cargo.lock") -Destination (Join-Path $buildRoot "Cargo.lock") -ErrorAction SilentlyContinue
    Copy-Item -LiteralPath (Join-Path $SourceRoot "src") -Destination (Join-Path $buildRoot "src") -Recurse

    $libPath = Join-Path $buildRoot "src\lib.rs"
    $lib = Get-Content -LiteralPath $libPath -Raw
    $lib = [regex]::Replace($lib, 'declare_id!\("[^"]+"\);', "declare_id!(`"$ProgramId`");", 1)
    Set-Content -LiteralPath $libPath -Value $lib -Encoding UTF8

    Invoke-CargoCapture -WorkingDirectory $benchRoot -Arguments @(
        "build-sbf",
        "--manifest-path",
        (Join-Path $buildRoot "Cargo.toml")
    ) | Out-Null

    return $buildRoot
}

function Add-ProgramIdArgs {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Deployments,

        [Parameter(Mandatory = $true)]
        [ref]$RunnerArgs
    )

    foreach ($deployment in $Deployments) {
        switch ($deployment.framework) {
            "hopper" { $RunnerArgs.Value += @("--hopper-program-id", $deployment.programId) }
            "pinocchio" { $RunnerArgs.Value += @("--pinocchio-program-id", $deployment.programId) }
            "quasar" { $RunnerArgs.Value += @("--quasar-program-id", $deployment.programId) }
            "anchor" { $RunnerArgs.Value += @("--anchor-program-id", $deployment.programId) }
        }
    }
}

if (-not $NoBuild) {
    # In-tree baselines: always built.
    Invoke-CargoCapture -WorkingDirectory $resolvedHopperRoot -Arguments @("build-sbf", "--manifest-path", "examples/hopper-parity-vault/Cargo.toml") | Out-Null
    Invoke-CargoCapture -WorkingDirectory $benchRoot -Arguments @("build-sbf", "--manifest-path", "pinocchio-vault/Cargo.toml") | Out-Null

    $anchorManifest = Join-Path $benchRoot "anchor-vault\Cargo.toml"
    if ((-not $DeployDevnet) -and (Test-Path -LiteralPath $anchorManifest)) {
        Invoke-CargoCapture -WorkingDirectory $benchRoot -Arguments @("build-sbf", "--manifest-path", "anchor-vault/Cargo.toml") | Out-Null
    }

    # Optional external comparators.
    if ($resolvedQuasarRoot) {
        Invoke-CargoCapture -WorkingDirectory $resolvedQuasarRoot -Arguments @("build-sbf", "--manifest-path", "examples/vault/Cargo.toml") | Out-Null
    }
}

$deployments = @()
$runnerAnchorRoot = $resolvedAnchorRoot
if ($DeployDevnet) {
    if (-not $Keypair) {
        throw "-DeployDevnet requires -Keypair <deployer.json>"
    }
    if (-not (Test-Path -LiteralPath $Keypair)) {
        throw "devnet keypair does not exist: $Keypair"
    }
    foreach ($tool in @("solana", "solana-keygen")) {
        if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
            throw "$tool is not on PATH"
        }
    }

    $deployDir = Join-Path $resolvedOutDir "devnet-programs"
    New-Item -ItemType Directory -Force -Path $deployDir | Out-Null

    $artifactSpecs = @(
        [ordered]@{ framework = "hopper"; binaryPath = Join-Path $resolvedHopperRoot "target\deploy\hopper_parity_vault.so" },
        [ordered]@{ framework = "pinocchio"; binaryPath = Join-Path $benchRoot "target\deploy\pinocchio_vault.so" }
    )
    if ($resolvedQuasarRoot) {
        $artifactSpecs += [ordered]@{ framework = "quasar"; binaryPath = Join-Path $resolvedQuasarRoot "target\deploy\quasar_vault.so" }
    }
    $anchorSourceRoot = Join-Path $benchRoot "anchor-vault"
    if (Test-Path -LiteralPath (Join-Path $anchorSourceRoot "Cargo.toml")) {
        $anchorProgramKeypair = New-ProgramKeypair -Framework "anchor" -DeployDir $deployDir
        $anchorProgramId = (Invoke-ExternalCapture -FilePath "solana-keygen" -Arguments @("pubkey", $anchorProgramKeypair)).Trim()
        $runnerAnchorRoot = New-AnchorDevnetBuildRoot -SourceRoot $anchorSourceRoot -DeployDir $deployDir -ProgramId $anchorProgramId
        $anchorArtifact = Join-Path $runnerAnchorRoot "target\deploy\anchor_vault.so"
        $artifactSpecs += [ordered]@{ framework = "anchor"; binaryPath = $anchorArtifact; programKeypair = $anchorProgramKeypair }
    }

    foreach ($artifact in $artifactSpecs) {
        $deployments += Deploy-FrameworkProgram -Framework $artifact.framework -BinaryPath $artifact.binaryPath -DeployDir $deployDir -ProgramKeypair $artifact.programKeypair
    }

    $manifestPath = Join-Path $resolvedOutDir "devnet-programs.json"
    [ordered]@{
        rpcUrl = $RpcUrl
        payer = (Invoke-ExternalCapture -FilePath "solana" -Arguments @("--keypair", $Keypair, "--url", $RpcUrl, "address")).Trim()
        deployments = $deployments
    } | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $manifestPath -Encoding UTF8
    Write-Host "Wrote $manifestPath" -ForegroundColor Green
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
if ($runnerAnchorRoot) {
    $runnerArgs += @("--anchor-root", $runnerAnchorRoot)
}
if ($deployments.Count -gt 0) {
    Add-ProgramIdArgs -Deployments $deployments -RunnerArgs ([ref]$runnerArgs)
}

$output = Invoke-CargoCapture -WorkingDirectory $benchRoot -Arguments $runnerArgs

Write-Host ""
Write-Host $output
