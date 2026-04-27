# hopper-bench

Compute-unit benchmarks and regression baselines for Hopper.

This repository is the separate benchmark product for the main Hopper framework
repo:

https://github.com/BluefootLabs/Hopper-Solana-Zero-copy-State-Framework

## Status

> **Benchmark lab.** Numbers in this repo are for local regression tracking and
> should be regenerated before publishing public performance claims.

The main Hopper framework crates were folded back into the main repo. This repo
stays separate because the benchmark lab has a different lifecycle, dependencies,
and CI surface.

## Layout

| Path | Purpose |
|---|---|
| `cu_baselines.toml` | Golden CU baselines for regression gates. |
| `hopper-bench` | On-chain primitive benchmark program. |
| `framework-vault-bench` | Shared Mollusk runner for cross-framework vault scenarios. |
| `pinocchio-vault` | In-tree Anza Pinocchio baseline. |
| `anchor-vault` | Optional Anchor comparator. |
| `lazy-dispatch-vault` | Hopper eager vs lazy entrypoint benchmark. |
| `docker` | Local validator docker-compose setup. |
| `measure.sh` / `measure.ps1` | Local benchmark wrappers. |
| `run-bench-docker.sh` / `run-bench-docker.ps1` | Docker-backed benchmark runners. |

## Running locally

### Docker validator

```powershell
# Windows
.\run-bench-docker.ps1
.\run-bench-docker.ps1 --no-build --out-dir results
```

```bash
# Linux / macOS / WSL
./run-bench-docker.sh
./run-bench-docker.sh --no-build --out-dir results
```

### Manual validator

```bash
solana-test-validator --reset
hopper profile bench
```

The thin wrappers `measure.sh` and `measure.ps1` delegate to
`hopper profile bench` when a local validator is already running.

## Cross-framework vault comparison

The vault comparison builds the in-tree Hopper and Pinocchio parity targets and
runs them under one Mollusk harness. Quasar and Anchor are optional and require
explicit local checkout roots.

```powershell
# Minimal: Hopper vs Pinocchio only.
.\compare-framework-vaults.ps1

# Include Quasar from a local checkout.
.\compare-framework-vaults.ps1 -QuasarRoot d:\tmp\framework-sources\quasar
```

The benchmark flow:

- builds the Hopper parity vault from the main Hopper checkout,
- builds the in-tree `pinocchio-vault`,
- optionally builds Quasar and Anchor comparators,
- runs shared deterministic user seed cases,
- writes JSON and CSV metrics under `results/framework-vaults`.

See [METHODOLOGY.md](METHODOLOGY.md) for scenario details and [AUDIT.md](AUDIT.md)
for the benchmark-audit notes that were moved with this repo.

## License

MIT OR Apache-2.0, matching Hopper.
