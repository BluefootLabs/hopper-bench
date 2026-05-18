# Cross-Framework Benchmark Methodology

Closes Hopper Safety Audit item **D4** ("Benchmark suite across frameworks").
This document is the ground truth for how cross-framework measurements
are taken, how results are reported, and how a contributor reproduces them.

## Goals

The audit's methodology requirements (page 15) are:

1. **Same toolchain**. every framework is compiled with the same
   `rustc` version and the same SBF toolchain release.
2. **Same optimization settings**. every framework is built with the
   same `cargo` profile flags (`release`, `opt-level=3`, `lto=fat`).
3. **Equivalent logic**. every reported cell uses the *same* public behaviour
   at the same call site. Frameworks are allowed to be idiomatic, but they are
   **not** allowed to change what the program actually does. If an upstream
   comparator does not implement a workload, that cell is `null` / `n/a`, not a
   synthesized substitute.
4. **Both host and SBF measurements** where relevant. On-chain
   CU is the authoritative comparison number; host benchmarks are
   used only for development iteration.

## Frameworks measured

| Framework | Binary source | Required |
|---|---|---|
| **Hopper** | `$hopper_root/target/deploy/hopper_parity_vault.so` (main Hopper repo) | Yes. baseline |
| **Pinocchio** | `target/deploy/pinocchio_vault.so` (this repo, `pinocchio-vault`) | Yes. raw-substrate baseline |
| **Quasar** | `$quasar_root/target/deploy/quasar_vault.so` | Optional, skipped if `--quasar-root` is not passed or the binary is missing |
| **Anchor** | `bench/anchor-vault/target/deploy/anchor_vault.so` (in-tree, R9) or `$anchor_root/target/deploy/anchor_vault.so` | Optional. Built in-tree via `cargo build-sbf --manifest-path bench/anchor-vault/Cargo.toml`; if that binary is missing, harness falls back to `--anchor-root` |

The Pinocchio baseline is now built in-tree against Anza's own
`pinocchio = "0.10"` and `pinocchio-system = "0.5"` crates
(see `pinocchio-vault/src/lib.rs`). Pre-R2 the Pinocchio column
was labelled "Pinocchio-style" and loaded from Quasar's third-party
reference vault; that indirection made the comparison ambiguous and is
removed. A framework without a built `.so` is silently skipped in the
emitted report so partial runs are valid during development. CI
requires every framework slot to be present for a release cut.

## Shared vault contract

The core vault contract is `deposit` and `withdraw`; every included framework
must implement those rows to appear in the table. Extended validation workloads
(`authorize`, `counter_access`) are reported only for frameworks whose benchmark
target implements them. This is the hard rule: behavioral equivalence is what
makes each compute-unit delta meaningful.

| Instruction | Behaviour | Accounts |
|---|---|---|
| `authorize` | Gate: require signer authority matches the vault's recorded authority | `[vault (ro), authority (signer)]` |
| `counter_access` | Read: increment a stored `u64` counter, return the new value | `[vault (mut), authority (signer)]` |
| `deposit` | Financial: move `lamports` from payer to vault, increment balance counter | `[vault (mut), payer (signer, mut), system_program]` |
| `withdraw` | Financial: move `lamports` from vault to user, require signer match | `[vault (mut), authority (signer), user (mut)]` |

Quasar's upstream `examples/vault` currently implements only `deposit` and
`withdraw`; the runner records `null` for Quasar's `authorize` and
`counter_access` fields. Hopper, the in-tree Anza Pinocchio target, and the
in-tree Anchor target implement the extended rows.

The vault state layout is:

```text
// 8 bytes counter + 32 bytes authority = 40 bytes body
#[repr(C)]
struct Vault {
    counter: u64,         // LE
    authority: [u8; 32],
}
```

Under the 16-byte Hopper header the total account size is 56 bytes
(8 + 32 + 16 header = 56). Competitor frameworks are free to use
their own header scheme but the *payload* semantics must match.

## Seven measured workloads (audit page 15)

Every workload is executed once per sample and the mean is reported.
Default `samples = 128`.

| Workload | What it exercises |
|---|---|
| Counter increment with one small state account | Baseline framework overhead |
| Two non-overlapping segment writes in one account | Hopper's segment-lease innovation |
| Sequential read-then-write of same segment | Exposes sticky-vs-RAII borrow design |
| PDA create + initialise + write | DX + lifecycle overhead |
| 64 KiB account scan (zero-copy read) | Fast-path advantage at scale |
| 1 MiB account scan (zero-copy read) | Extreme-size behaviour |
| Cross-program foreign-field read | Lens / ABI verification cost |

The vault runner maps to the `authorize` / `counter_access` / `deposit` /
`withdraw` instructions when a framework target implements them. Workloads 5-7 live in the
`bench/hopper-bench` on-chain program (they don't need cross-framework
equivalents because every framework reduces to raw byte reads for
them).

## What is measured

Per framework, per supported instruction:

- **Compute units**. read from `sol_log_compute_units` deltas.
- **Binary size**. from the `.so` file on disk, both in bytes and KiB.
- **Stack frame size**. extracted from `llvm-objdump --section=.text`.
- **Unsigned-withdraw rejection**. a safety correctness check: the
  `withdraw` instruction *must* reject when the signer constraint is
  violated. Any framework that accepts it fails this row.

Results are emitted as a JSON report plus a CSV under
`results/framework-vaults` by default for spreadsheet use.
The Hopper row is the baseline (delta = 0); every other row reports
`cu_delta = framework_cu - hopper_cu` so the direction of the
comparison is unambiguous.

## Pinning

The benchmark output is reproducible iff every node in the chain is
pinned:

| Component | Where pinned |
|---|---|
| rustc version | `rust-toolchain.toml` (repo root) |
| SBF toolchain | `bench/Dockerfile` or contributor's `solana --version` output |
| cargo profile | `bench/framework-vault-bench/Cargo.toml` `[profile.release]` |
| mollusk-svm | `bench/framework-vault-bench/Cargo.toml` version constraint |
| competitor commits | `bench/competitors.lock` (new; records git SHAs) |

Contributors should snapshot their `solana --version` and the
upstream competitor commits into `bench/competitors.lock` before
publishing any cross-framework numbers.

## Running the cross-framework bench

```bash
# Build the in-tree baselines. Hopper and Pinocchio are both local;
# Quasar and Anchor are external and optional.
cargo build-sbf --manifest-path ../Hopper-Solana-Zero-copy-State-Framework/examples/hopper-parity-vault/Cargo.toml
cargo build-sbf --manifest-path pinocchio-vault/Cargo.toml
(cd $QUASAR_ROOT && cargo build-sbf -p quasar-vault)       # optional
cargo build-sbf --manifest-path anchor-vault/Cargo.toml  # optional in-tree Anchor (R9)
(cd $ANCHOR_ROOT && anchor build -p anchor-vault)          # optional external

# Run the shared harness. `--quasar-root` and `--anchor-root` are
# both optional; pass either flag to include that framework in the
# matrix.
cargo run -p framework-vault-bench --release -- \
   --hopper-root ../Hopper-Solana-Zero-copy-State-Framework \
    --quasar-root $QUASAR_ROOT \
    --anchor-root $ANCHOR_ROOT \
      --out-dir results/framework-vaults

# Inspect.
jq '.benchmarks[] | {framework, authorize_cu, deposit_cu, binary_size_kib}' \
      results/framework-vaults/vault-framework-comparison.json
```

   The PowerShell wrapper `compare-framework-vaults.ps1` encapsulates the same
   flow for local release runs. Docker-backed runs live under `docker/`.

## Adding a new framework

1. Build the framework's vault `.so` with the shared contract above.
2. Add an entry to `ProgramSpec` in `bench/framework-vault-bench/src/main.rs`
   with the framework name, program ID, and binary path.
3. Run the harness. the new framework auto-appears in the report
   with `cu_delta` computed against the Hopper baseline.
4. Document the new competitor's commit SHA in `bench/competitors.lock`.

## Safety-correctness gate

Any framework that fails `unsigned_withdraw_rejected` is recorded in the report
but **excluded** from CU-delta totals. A framework that's faster because it
skipped a safety check is not a meaningful comparison. The report flags this
explicitly so readers know.

## Interpreting CU deltas

Small deltas (under 50 CU) are within run-to-run noise on Mollusk. Differences
under 100 CU are uninteresting for most protocol decisions. Differences at the
500+ CU level reflect genuine implementation or framework-level overhead. The
cross-framework report validates this vault contract; it is not a universal
claim about every possible Pinocchio or Quasar program.
