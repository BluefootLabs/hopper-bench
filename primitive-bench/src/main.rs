//! Mollusk host runner for the Hopper primitive CU micro-benchmark.
//!
//! Drives the on-chain `hopper-bench` program (discs 0..=21, see
//! `hopper-bench/src/lib.rs`) under mollusk-svm 0.10.3 and reports TWO
//! CU columns per primitive:
//!
//! 1. `whole_ix_cu` — `compute_units_consumed` for the entire
//!    instruction (dispatch + fixture checks + logging included).
//! 2. `bracketed_cu` — the delta between the two
//!    `sol_log_compute_units()` lines the program emits around the
//!    measured operation, parsed from the Mollusk `LogCollector`. This
//!    matches the historical validator-log measurement method, so the
//!    numbers stay comparable with the old BENCHMARKS.md figures.
//!    Note: like the validator method, the delta includes the cost of
//!    the closing `sol_log_compute_units` syscall itself; disc 21
//!    (`measurement_overhead`) measures exactly that bracket overhead,
//!    and the `net_cu` column subtracts it.
//!
//! Fixture strategy: header-bearing discs need account data with a
//! valid Hopper header (disc / version / layout-id fingerprint). Rather
//! than replicating the macro fingerprint hash on the host, the runner
//! BOOTSTRAPS the data by first invoking disc 6 (`write_header`, for
//! `BenchVault`) and disc 20 (`write_proc_header`, for
//! `ProcBenchVault`) against zeroed accounts and capturing the
//! resulting account bytes.
//!
//! Modeled on `router-bench/src/main.rs` (same Mollusk + fixture
//! conventions).

use {
    mollusk_svm::Mollusk,
    solana_account::Account,
    solana_address::Address,
    solana_instruction::{AccountMeta, Instruction},
    solana_svm_log_collector::LogCollector,
    std::{
        env, fs,
        path::{Path, PathBuf},
    },
};

/// Arbitrary fixed program id for the bench program.
const PROGRAM_ID: Address = Address::new_from_array([0x7B; 32]);

/// Fixture account address (owned by the bench program).
const VAULT_ADDRESS: Address = Address::new_from_array([0x11; 32]);
/// Separate fixture for the proc-macro vault (disc 19/20).
const PROC_VAULT_ADDRESS: Address = Address::new_from_array([0x22; 32]);

/// `BenchVault` wire size: 16-byte Hopper header + 32 + 8 + 1 fields.
const BENCH_VAULT_LEN: usize = 57;
/// `ProcBenchVault` wire size: 16-byte header + two `WireU64` fields.
const PROC_VAULT_LEN: usize = 32;

const FIXTURE_LAMPORTS: u64 = 1_000_000_000;

/// What account shape a disc needs.
#[derive(Clone, Copy)]
enum Fixture {
    /// One `BenchVault`-shaped account (signer + writable + program-owned,
    /// data bootstrapped with a valid header via disc 6).
    Vault,
    /// The vault account passed twice (disc 4, `check_keys_eq`).
    VaultTwice,
    /// One `ProcBenchVault`-shaped account (bootstrapped via disc 20).
    ProcVault,
    /// No accounts at all (disc 9 `emit_event`, disc 21 overhead).
    None,
}

struct DiscSpec {
    disc: u8,
    name: &'static str,
    fixture: Fixture,
    /// Extra instruction-data bytes after the disc byte.
    extra_data: &'static [u8],
    /// April BENCHMARKS.md "Expected CU" claim (None where none existed).
    stale_claim_cu: Option<u64>,
}

/// disc 19 argument: `amount = 1` as u64 LE.
const PROC_DEPOSIT_AMOUNT: [u8; 8] = 1u64.to_le_bytes();

const SPECS: &[DiscSpec] = &[
    DiscSpec {
        disc: 0,
        name: "check_signer",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(20),
    },
    DiscSpec {
        disc: 1,
        name: "check_writable",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(20),
    },
    DiscSpec {
        disc: 2,
        name: "check_owner",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(50),
    },
    DiscSpec {
        disc: 3,
        name: "check_account_tier1 (load)",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(120),
    },
    DiscSpec {
        disc: 4,
        name: "check_keys_eq",
        fixture: Fixture::VaultTwice,
        extra_data: &[],
        stale_claim_cu: Some(40),
    },
    DiscSpec {
        disc: 5,
        name: "overlay (57B)",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(8),
    },
    DiscSpec {
        disc: 6,
        name: "write_header",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(30),
    },
    DiscSpec {
        disc: 7,
        name: "zero_init (57B)",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(15),
    },
    DiscSpec {
        disc: 8,
        name: "check_account_fast",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(12),
    },
    DiscSpec {
        disc: 9,
        name: "emit_event (32B)",
        fixture: Fixture::None,
        extra_data: &[],
        stale_claim_cu: Some(100),
    },
    DiscSpec {
        disc: 10,
        name: "trust_strict_load",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(130),
    },
    DiscSpec {
        disc: 11,
        name: "pod_from_bytes (57B)",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(6),
    },
    DiscSpec {
        disc: 12,
        name: "receipt_begin_commit (57B)",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(50),
    },
    DiscSpec {
        disc: 13,
        name: "fingerprint_check",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(15),
    },
    DiscSpec {
        disc: 14,
        name: "state_diff (57B)",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(30),
    },
    DiscSpec {
        disc: 15,
        name: "overlay_mut + field_set",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(10),
    },
    DiscSpec {
        disc: 16,
        name: "raw_cast_baseline (unsafe)",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(4),
    },
    DiscSpec {
        disc: 17,
        name: "receipt_full (enriched)",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(80),
    },
    DiscSpec {
        disc: 18,
        name: "receipt_emit (64B log)",
        fixture: Fixture::Vault,
        extra_data: &[],
        stale_claim_cu: Some(150),
    },
    DiscSpec {
        disc: 19,
        name: "proc_macro_typed_dispatch",
        fixture: Fixture::ProcVault,
        extra_data: &PROC_DEPOSIT_AMOUNT,
        stale_claim_cu: Some(80),
    },
    DiscSpec {
        disc: 20,
        name: "write_proc_header (unbracketed)",
        fixture: Fixture::ProcVault,
        extra_data: &[],
        stale_claim_cu: None,
    },
    DiscSpec {
        disc: 21,
        name: "measurement_overhead",
        fixture: Fixture::None,
        extra_data: &[],
        stale_claim_cu: None,
    },
];

struct Row {
    disc: u8,
    name: &'static str,
    whole_ix_cu: u64,
    bracketed_cu: Option<u64>,
    net_cu: Option<u64>,
    stale_claim_cu: Option<u64>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let (so_path, out_dir) = parse_args()?;
    ensure_file(&so_path).map_err(|msg| {
        format!("{msg} (build it with `cargo build-sbf --manifest-path hopper-bench/Cargo.toml`)")
    })?;

    let mut mollusk = Mollusk::default();
    mollusk.add_program(&PROGRAM_ID, &program_path_stem(&so_path));

    // Bootstrap header-bearing account data on-chain (disc 6 / disc 20)
    // so the host never has to reproduce the layout-id fingerprint.
    let vault_data = bootstrap_data(&mut mollusk, 6, VAULT_ADDRESS, BENCH_VAULT_LEN)?;
    let proc_vault_data = bootstrap_data(&mut mollusk, 20, PROC_VAULT_ADDRESS, PROC_VAULT_LEN)?;

    let mut rows = Vec::with_capacity(SPECS.len());
    for spec in SPECS {
        // Run twice; the fixture is deterministic so CU must be stable.
        let first = run_disc(&mut mollusk, spec, &vault_data, &proc_vault_data)?;
        let second = run_disc(&mut mollusk, spec, &vault_data, &proc_vault_data)?;
        if first.0 != second.0 || first.1 != second.1 {
            return Err(format!(
                "disc {} ({}) is not CU-stable across identical runs: {:?} vs {:?}",
                spec.disc, spec.name, first, second
            ));
        }
        rows.push(Row {
            disc: spec.disc,
            name: spec.name,
            whole_ix_cu: first.0,
            bracketed_cu: first.1,
            net_cu: None,
            stale_claim_cu: spec.stale_claim_cu,
        });
    }

    // Bracket overhead = disc 21's bracketed delta (an empty bracket:
    // just the closing sol_log_compute_units syscall).
    let overhead = rows
        .iter()
        .find(|row| row.disc == 21)
        .and_then(|row| row.bracketed_cu)
        .ok_or_else(|| "disc 21 (measurement_overhead) produced no bracketed delta".to_string())?;
    for row in &mut rows {
        row.net_cu = row.bracketed_cu.map(|cu| cu.saturating_sub(overhead));
    }

    let markdown = markdown_report(&rows, overhead, &so_path);
    let csv = csv_report(&rows);

    fs::create_dir_all(&out_dir)
        .map_err(|err| format!("failed to create {}: {err}", out_dir.display()))?;
    let md_path = out_dir.join("primitive-cu.md");
    let csv_path = out_dir.join("primitive-cu.csv");
    fs::write(&md_path, &markdown)
        .map_err(|err| format!("failed to write {}: {err}", md_path.display()))?;
    fs::write(&csv_path, &csv)
        .map_err(|err| format!("failed to write {}: {err}", csv_path.display()))?;

    print!("{markdown}");
    println!();
    println!("Wrote {}", md_path.display());
    println!("Wrote {}", csv_path.display());
    Ok(())
}

fn parse_args() -> Result<(PathBuf, PathBuf), String> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "failed to resolve workspace root".to_string())?;
    let mut so_path = workspace_root.join("target/deploy/hopper_bench.so");
    let mut out_dir = workspace_root.join("results/primitive-bench");

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--so-path" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --so-path".to_string())?;
                so_path = PathBuf::from(value);
            }
            "--out-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --out-dir".to_string())?;
                out_dir = PathBuf::from(value);
            }
            _ => {
                return Err(format!(
                    "unknown argument '{arg}'; expected [--so-path <hopper_bench.so>] [--out-dir <path>]"
                ));
            }
        }
    }
    Ok((so_path, out_dir))
}

fn ensure_file(path: &Path) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("missing benchmark artifact {}", path.display()))
    }
}

/// mollusk-svm 0.10.3 `add_program` appends `.so` itself.
fn program_path_stem(path: &Path) -> String {
    let mut stem = path.to_path_buf();
    stem.set_extension("");
    stem.display().to_string()
}

/// A program-owned, signer, writable fixture account. Signer + writable
/// are over-provisioned on purpose so one shape satisfies every check
/// primitive; Mollusk does not verify signatures.
fn fixture_account(data: Vec<u8>) -> Account {
    Account {
        lamports: FIXTURE_LAMPORTS,
        data,
        owner: PROGRAM_ID,
        executable: false,
        rent_epoch: 0,
    }
}

fn instruction_data(disc: u8, extra: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + extra.len());
    data.push(disc);
    data.extend_from_slice(extra);
    data
}

/// Invoke a header-writing disc against a zeroed account and return the
/// resulting account bytes (now carrying a valid Hopper header).
fn bootstrap_data(
    mollusk: &mut Mollusk,
    disc: u8,
    address: Address,
    len: usize,
) -> Result<Vec<u8>, String> {
    let instruction = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![AccountMeta::new(address, true)],
        data: instruction_data(disc, &[]),
    };
    let accounts = vec![(address, fixture_account(vec![0u8; len]))];
    let result = mollusk.process_instruction(&instruction, &accounts);
    if !result.program_result.is_ok() {
        return Err(format!(
            "bootstrap disc {disc} failed: {:?}",
            result.program_result
        ));
    }
    result
        .resulting_accounts
        .iter()
        .find(|(key, _)| *key == address)
        .map(|(_, account)| account.data.clone())
        .ok_or_else(|| format!("bootstrap disc {disc}: fixture account missing from result"))
}

/// Run one disc; returns (whole-instruction CU, bracketed log delta).
fn run_disc(
    mollusk: &mut Mollusk,
    spec: &DiscSpec,
    vault_data: &[u8],
    proc_vault_data: &[u8],
) -> Result<(u64, Option<u64>), String> {
    let (metas, accounts): (Vec<AccountMeta>, Vec<(Address, Account)>) = match spec.fixture {
        Fixture::Vault => (
            vec![AccountMeta::new(VAULT_ADDRESS, true)],
            vec![(VAULT_ADDRESS, fixture_account(vault_data.to_vec()))],
        ),
        Fixture::VaultTwice => (
            // Same address twice: check_keys_eq(accounts[0], accounts[1])
            // must see equal keys.
            vec![
                AccountMeta::new(VAULT_ADDRESS, true),
                AccountMeta::new(VAULT_ADDRESS, true),
            ],
            vec![(VAULT_ADDRESS, fixture_account(vault_data.to_vec()))],
        ),
        Fixture::ProcVault => (
            vec![AccountMeta::new(PROC_VAULT_ADDRESS, true)],
            vec![(
                PROC_VAULT_ADDRESS,
                fixture_account(proc_vault_data.to_vec()),
            )],
        ),
        Fixture::None => (Vec::new(), Vec::new()),
    };

    let instruction = Instruction {
        program_id: PROGRAM_ID,
        accounts: metas,
        data: instruction_data(spec.disc, spec.extra_data),
    };

    // Fresh log collector per invocation so consumption lines from
    // earlier discs cannot bleed into this parse.
    let logger = LogCollector::new_ref();
    mollusk.logger = Some(logger.clone());
    let result = mollusk.process_instruction(&instruction, &accounts);
    mollusk.logger = None;

    if !result.program_result.is_ok() {
        return Err(format!(
            "disc {} ({}) failed: {:?}",
            spec.disc, spec.name, result.program_result
        ));
    }

    let logs: Vec<String> = logger.borrow().get_recorded_content().to_vec();
    let bracketed = bracketed_delta(&logs)?;
    Ok((result.compute_units_consumed, bracketed))
}

/// Parse the `sol_log_compute_units` bracket from program logs.
///
/// The runtime renders each call as
/// `Program consumption: <N> units remaining`; the bench program emits
/// exactly two per bracketed disc (and zero for unbracketed ones).
/// Delta = first_remaining - second_remaining, identical to the
/// historical validator-log method.
fn bracketed_delta(logs: &[String]) -> Result<Option<u64>, String> {
    let mut remaining = Vec::new();
    for line in logs {
        if let Some(rest) = line.strip_prefix("Program consumption: ") {
            if let Some(value) = rest.strip_suffix(" units remaining") {
                let parsed = value
                    .parse::<u64>()
                    .map_err(|err| format!("bad consumption line `{line}`: {err}"))?;
                remaining.push(parsed);
            }
        }
    }
    match remaining.as_slice() {
        [] => Ok(None),
        [first, second] => Ok(Some(first.checked_sub(*second).ok_or_else(|| {
            format!("consumption bracket went backwards: {first} -> {second}")
        })?)),
        other => Err(format!(
            "expected 0 or 2 consumption lines, found {}",
            other.len()
        )),
    }
}

fn fmt_opt(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_string(), |v| v.to_string())
}

fn markdown_report(rows: &[Row], overhead: u64, so_path: &Path) -> String {
    let mut out = String::new();
    out.push_str("# Hopper primitive CU micro-benchmark (Mollusk)\n\n");
    out.push_str(&format!(
        "Runner: `primitive-bench` (mollusk-svm 0.10.3, validator-free). Program: `{}`.\n\n",
        so_path.display()
    ));
    out.push_str(
        "- `whole_ix_cu`: compute_units_consumed for the full instruction \
         (dispatch + fixture checks + BEGIN/END logging included).\n",
    );
    out.push_str(
        "- `bracketed_cu`: delta between the two `sol_log_compute_units` lines, \
         parsed from program logs — same method as the historical validator \
         measurements. Includes the closing syscall's own cost.\n",
    );
    out.push_str(&format!(
        "- `net_cu`: bracketed_cu minus the empty-bracket overhead measured by \
         disc 21 ({overhead} CU) — the closest estimate of the primitive alone.\n",
    ));
    out.push_str(
        "- `stale_apr_claim`: the April BENCHMARKS.md \"Expected CU\" figure being replaced.\n\n",
    );
    out.push_str("| Disc | Primitive | whole_ix_cu | bracketed_cu | net_cu | stale_apr_claim |\n");
    out.push_str("|------|-----------|-------------|--------------|--------|------------------|\n");
    for row in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            row.disc,
            row.name,
            row.whole_ix_cu,
            fmt_opt(row.bracketed_cu),
            fmt_opt(row.net_cu),
            row.stale_claim_cu
                .map_or_else(|| "—".to_string(), |v| format!("~{v}")),
        ));
    }
    out
}

fn csv_report(rows: &[Row]) -> String {
    let mut out =
        String::from("disc,primitive,whole_ix_cu,bracketed_cu,net_cu,stale_apr_claim_cu\n");
    for row in rows {
        out.push_str(&format!(
            "{},{},{},{},{},{}\n",
            row.disc,
            row.name.replace(',', ";"),
            row.whole_ix_cu,
            fmt_opt(row.bracketed_cu).replace('—', ""),
            fmt_opt(row.net_cu).replace('—', ""),
            row.stale_claim_cu
                .map_or_else(String::new, |v| v.to_string()),
        ));
    }
    out
}
