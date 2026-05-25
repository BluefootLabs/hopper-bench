use {
    mollusk_svm::{program::keyed_account_for_system_program, Mollusk},
    serde::Serialize,
    solana_account::Account,
    solana_address::Address,
    solana_instruction::{AccountMeta, Instruction},
    std::{
        env, fs,
        path::{Path, PathBuf},
        str::FromStr,
    },
};

const HOPPER_PROGRAM_ID: Address = Address::new_from_array([7; 32]);
const QUASAR_PROGRAM_ID: Address = Address::new_from_array(five8_const::decode_32_const(
    "33333333333333333333333333333333333333333333",
));
const PINOCCHIO_PROGRAM_ID: Address = Address::new_from_array([
    0x1e, 0x3c, 0xd6, 0x28, 0x43, 0x80, 0x94, 0x0e, 0x08, 0x62, 0x4c, 0xb8, 0x33, 0x8b, 0x77, 0xdc,
    0x33, 0x25, 0x75, 0xd1, 0x5f, 0xa3, 0x9a, 0x0f, 0x1d, 0xf1, 0x5e, 0xe0, 0x8f, 0xb8, 0x23, 0xee,
]);
// Deterministic Anchor-framework program ID for the cross-framework
// comparison matrix. Distinct from the other slots so Mollusk can
// distinguish calls. See `bench/METHODOLOGY.md` for the shared
// contract the anchor vault must implement.
const ANCHOR_PROGRAM_ID: Address = Address::new_from_array([
    0xa7, 0xcb, 0x04, 0x12, 0xe8, 0x55, 0x6f, 0x91, 0x3b, 0x0c, 0xaa, 0xfe, 0x22, 0x40, 0x44, 0x56,
    0x8d, 0x1e, 0x63, 0x7a, 0x04, 0xcd, 0x15, 0x87, 0xbe, 0xef, 0x00, 0x00, 0xfa, 0xde, 0xd1, 0xee,
]);
const SYSTEM_PROGRAM_ID: Address = Address::new_from_array([0; 32]);

const USER_LAMPORTS: u64 = 10_000_000_000;
const DEPOSIT_AMOUNT: u64 = 1_000_000_000;
const WITHDRAW_VAULT_LAMPORTS: u64 = 1_000_000_000;
const WITHDRAW_AMOUNT: u64 = 500_000_000;
const COUNTER_DATA_LEN: usize = 40;
const ANCHOR_COUNTER_DATA_LEN: usize = 48;
const ANCHOR_COUNTER_STATE_DISCRIMINATOR: [u8; 8] =
    [0x62, 0x17, 0xcd, 0x9f, 0x0e, 0xca, 0x4f, 0x8b];
const COUNTER_INITIAL_VALUE: u64 = 7;
const SHARED_USER_CASES: [[u8; 32]; 8] = [
    [0x11; 32], [0x22; 32], [0x33; 32], [0x44; 32], [0x55; 32], [0x66; 32], [0x77; 32], [0x88; 32],
];

#[derive(Serialize)]
struct Methodology {
    runner: &'static str,
    samples: usize,
    authorize: &'static str,
    authorize_failure: &'static str,
    counter_access: &'static str,
    deposit: &'static str,
    withdraw: &'static str,
    safety_check: &'static str,
}

#[derive(Serialize)]
struct FrameworkMetric {
    framework: &'static str,
    program_id: String,
    authorize_cu: Option<u64>,
    authorize_missing_signature_cu: Option<u64>,
    counter_access_cu: Option<u64>,
    deposit_cu: u64,
    withdraw_cu: u64,
    authorize_vs_hopper: Option<i64>,
    counter_access_vs_hopper: Option<i64>,
    deposit_vs_hopper: i64,
    withdraw_vs_hopper: i64,
    binary_size_bytes: u64,
    binary_size_kib: f64,
    unsigned_withdraw_rejected: bool,
}

#[derive(Serialize)]
struct Report {
    hopper_root: String,
    quasar_root: Option<String>,
    methodology: Methodology,
    benchmarks: Vec<FrameworkMetric>,
}

struct ProgramSpec {
    framework: &'static str,
    program_id: Address,
    binary_path: PathBuf,
    supports_validation_workloads: bool,
}

struct Args {
    /// Main Hopper framework checkout. The benchmark product lives in a
    /// separate repo, so Hopper artifacts are loaded from this path.
    hopper_root: PathBuf,
    /// Optional quasar-vault build root. When provided, a `quasar`
    /// framework entry is appended to the matrix. Quasar is an
    /// external project (blueshift-gg/quasar); pass `--quasar-root
    /// <path>` to include it. Pre-R2 this was required; it is now
    /// optional because the pinocchio baseline is built in-tree.
    quasar_root: Option<PathBuf>,
    /// Optional anchor-vault build root. When provided, an
    /// `anchor` framework entry is appended to the measurement
    /// matrix. See `bench/METHODOLOGY.md` for the shared contract.
    anchor_root: Option<PathBuf>,
    out_dir: PathBuf,
    program_ids: ProgramIdOverrides,
}

#[derive(Default)]
struct ProgramIdOverrides {
    hopper: Option<Address>,
    pinocchio: Option<Address>,
    quasar: Option<Address>,
    anchor: Option<Address>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let workspace_root = workspace_root()?;

    // Audit D4 closure: Hopper is the baseline. Pinocchio is the
    // raw-substrate baseline, built in-tree from `bench/pinocchio-vault`
    // so the artefact is unambiguously Anza Pinocchio and not a
    // third-party reference. Quasar is the framework-tier comparator,
    // loaded from `$quasar_root/target/deploy/quasar_vault.so`. Anchor
    // is optional; pass `--anchor-root <path>` and the anchor_vault.so
    // there is appended to the matrix. The runner does not auto-discover
    // in-tree Anchor artifacts because stale local builds can carry a
    // different declared program ID. Frameworks whose binaries are
    // missing are skipped and logged rather than erroring out; partial
    // runs are valid during development. CI builds require every
    // framework to be present (see `bench/METHODOLOGY.md`). See
    // `AUDIT.md` R2 for the rationale behind the column rename.
    let mut specs: Vec<ProgramSpec> = vec![
        ProgramSpec {
            framework: "hopper",
            program_id: args.program_ids.hopper.unwrap_or(HOPPER_PROGRAM_ID),
            binary_path: args
                .hopper_root
                .join("target/deploy/hopper_parity_vault.so"),
            supports_validation_workloads: true,
        },
        ProgramSpec {
            framework: "pinocchio",
            program_id: args.program_ids.pinocchio.unwrap_or(PINOCCHIO_PROGRAM_ID),
            binary_path: workspace_root.join("target/deploy/pinocchio_vault.so"),
            supports_validation_workloads: true,
        },
    ];
    if let Some(quasar_root) = &args.quasar_root {
        specs.push(ProgramSpec {
            framework: "quasar",
            program_id: args.program_ids.quasar.unwrap_or(QUASAR_PROGRAM_ID),
            binary_path: quasar_root.join("target/deploy/quasar_vault.so"),
            supports_validation_workloads: false,
        });
    }
    // Anchor is explicit-only so a stale in-tree artifact cannot silently
    // change a Hopper/Pinocchio/Quasar comparison matrix.
    if let Some(anchor_root) = &args.anchor_root {
        specs.push(ProgramSpec {
            framework: "anchor",
            program_id: args.program_ids.anchor.unwrap_or(ANCHOR_PROGRAM_ID),
            binary_path: anchor_root.join("target/deploy/anchor_vault.so"),
            supports_validation_workloads: true,
        });
    }

    // Hopper baseline is required. Other frameworks are skipped with
    // a log line if their artifact is missing.
    specs.retain(|spec| match ensure_file(&spec.binary_path) {
        Ok(()) => true,
        Err(msg) => {
            if spec.framework == "hopper" {
                // Keep the spec so the later baseline-lookup fails loudly.
                eprintln!("error: {msg}");
                true
            } else {
                eprintln!("skipping framework `{}`: {msg}", spec.framework);
                false
            }
        }
    });

    fs::create_dir_all(&args.out_dir).map_err(|err| {
        format!(
            "failed to create output directory {}: {err}",
            args.out_dir.display()
        )
    })?;

    let mut benchmarks = Vec::with_capacity(specs.len());
    for spec in &specs {
        benchmarks.push(run_program(spec)?);
    }

    let hopper_index = benchmarks
        .iter()
        .position(|benchmark| benchmark.framework == "hopper")
        .ok_or_else(|| "missing Hopper baseline in benchmark results".to_string())?;
    let hopper_authorize = benchmarks[hopper_index]
        .authorize_cu
        .ok_or_else(|| "Hopper baseline missing authorize workload".to_string())?;
    let hopper_counter_access = benchmarks[hopper_index]
        .counter_access_cu
        .ok_or_else(|| "Hopper baseline missing counter_access workload".to_string())?;
    let hopper_deposit = benchmarks[hopper_index].deposit_cu;
    let hopper_withdraw = benchmarks[hopper_index].withdraw_cu;

    for benchmark in &mut benchmarks {
        benchmark.authorize_vs_hopper = benchmark
            .authorize_cu
            .map(|cu| cu as i64 - hopper_authorize as i64);
        benchmark.counter_access_vs_hopper = benchmark
            .counter_access_cu
            .map(|cu| cu as i64 - hopper_counter_access as i64);
        benchmark.deposit_vs_hopper = benchmark.deposit_cu as i64 - hopper_deposit as i64;
        benchmark.withdraw_vs_hopper = benchmark.withdraw_cu as i64 - hopper_withdraw as i64;
    }

    let report = Report {
        hopper_root: args.hopper_root.display().to_string(),
        quasar_root: args
            .quasar_root
            .as_ref()
            .map(|p| p.display().to_string()),
        methodology: Methodology {
            runner: "mollusk-svm shared host runner loading every present framework's compiled SBF binary",
            samples: SHARED_USER_CASES.len(),
            authorize: "average across shared deterministic user seed cases; signer + writable + PDA validation only on the same ['vault', user] PDA shape with no CPI or lamport mutation; null when an upstream comparator does not implement this instruction",
            authorize_failure: "missing-signature variant of the authorize path; must fail without mutating balances and exposes the early validation cost; null when an upstream comparator does not implement authorize",
            counter_access: "average across shared deterministic user seed cases; same ['vault', user] PDA plus [authority:32][counter:8] state, with Anchor using its 8-byte AccountLoader discriminator in front of the same body; validated and incremented without CPI or lamport mutation; null when an upstream comparator does not implement this instruction",
            deposit: "average across shared deterministic user seed cases; user signer -> program-owned vault PDA via system-program transfer CPI with PDA seeds [\"vault\", user]",
            withdraw: "average across shared deterministic user seed cases; program-owned vault PDA -> user via direct lamport mutation after signer and PDA validation",
            safety_check: "unsigned withdraw must fail without mutating balances",
        },
        benchmarks,
    };

    let json_path = args.out_dir.join("vault-framework-comparison.json");
    let csv_path = args.out_dir.join("vault-framework-comparison.csv");

    fs::write(
        &json_path,
        serde_json::to_string_pretty(&report)
            .map_err(|err| format!("failed to serialize JSON report: {err}"))?,
    )
    .map_err(|err| format!("failed to write {}: {err}", json_path.display()))?;

    fs::write(&csv_path, csv_report(&report))
        .map_err(|err| format!("failed to write {}: {err}", csv_path.display()))?;

    println!();
    println!("Vault framework comparison");
    println!(
        "{:<16} {:<44} {:>10} {:>11} {:>10} {:>11} {:>10} {:>11} {:>11}",
        "Framework",
        "ProgramId",
        "Authorize",
        "AuthFail",
        "Counter",
        "Deposit",
        "Withdraw",
        "Binary KiB",
        "Safety"
    );
    for benchmark in &report.benchmarks {
        println!(
            "{:<16} {:<44} {:>10} {:>11} {:>10} {:>11} {:>10} {:>11.2} {:>11}",
            benchmark.framework,
            benchmark.program_id,
            fmt_opt_u64(benchmark.authorize_cu),
            fmt_opt_u64(benchmark.authorize_missing_signature_cu),
            fmt_opt_u64(benchmark.counter_access_cu),
            benchmark.deposit_cu,
            benchmark.withdraw_cu,
            benchmark.binary_size_kib,
            if benchmark.unsigned_withdraw_rejected {
                "rejected"
            } else {
                "FAILED"
            }
        );
    }
    println!();
    println!("Wrote {}", json_path.display());
    println!("Wrote {}", csv_path.display());

    Ok(())
}

fn parse_args() -> Result<Args, String> {
    let mut hopper_root = None;
    let mut quasar_root = None;
    let mut anchor_root: Option<PathBuf> = None;
    let mut out_dir = None;
    let mut program_ids = ProgramIdOverrides::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--hopper-root" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --hopper-root".to_string())?;
                hopper_root = Some(PathBuf::from(value));
            }
            "--quasar-root" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --quasar-root".to_string())?;
                quasar_root = Some(PathBuf::from(value));
            }
            "--anchor-root" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --anchor-root".to_string())?;
                anchor_root = Some(PathBuf::from(value));
            }
            "--out-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --out-dir".to_string())?;
                out_dir = Some(PathBuf::from(value));
            }
            "--hopper-program-id" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --hopper-program-id".to_string())?;
                program_ids.hopper = Some(parse_program_id("--hopper-program-id", &value)?);
            }
            "--pinocchio-program-id" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --pinocchio-program-id".to_string())?;
                program_ids.pinocchio = Some(parse_program_id("--pinocchio-program-id", &value)?);
            }
            "--quasar-program-id" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --quasar-program-id".to_string())?;
                program_ids.quasar = Some(parse_program_id("--quasar-program-id", &value)?);
            }
            "--anchor-program-id" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --anchor-program-id".to_string())?;
                program_ids.anchor = Some(parse_program_id("--anchor-program-id", &value)?);
            }
            _ => {
                return Err(format!(
                    "unknown argument '{arg}'; expected [--hopper-root <path>] [--quasar-root <path>] [--anchor-root <path>] [--out-dir <path>] [--hopper-program-id <id>] [--pinocchio-program-id <id>] [--quasar-program-id <id>] [--anchor-program-id <id>]"
                ));
            }
        }
    }

    let workspace_root = workspace_root()?;
    let hopper_root = hopper_root
        .map(|p| canonicalize_existing(&p))
        .transpose()?
        .unwrap_or_else(|| workspace_root.clone());
    let quasar_root = quasar_root.map(|p| canonicalize_existing(&p)).transpose()?;
    let anchor_root = anchor_root.map(|p| canonicalize_existing(&p)).transpose()?;
    let out_dir = match out_dir {
        Some(path) if path.is_absolute() => path,
        Some(path) => workspace_root.join(path),
        None => workspace_root.join("bench/results/framework-vaults"),
    };

    Ok(Args {
        hopper_root,
        quasar_root,
        anchor_root,
        out_dir,
        program_ids,
    })
}

fn parse_program_id(flag: &str, value: &str) -> Result<Address, String> {
    Address::from_str(value).map_err(|err| format!("invalid {flag} value `{value}`: {err}"))
}

fn workspace_root() -> Result<PathBuf, String> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().map(Path::to_path_buf).ok_or_else(|| {
        format!(
            "failed to resolve workspace root from {}",
            manifest_dir.display()
        )
    })
}

fn canonicalize_existing(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|err| format!("failed to resolve {}: {err}", path.display()))
}

fn ensure_file(path: &Path) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("missing benchmark artifact {}", path.display()))
    }
}

fn run_program(spec: &ProgramSpec) -> Result<FrameworkMetric, String> {
    let authorize_cu = if spec.supports_validation_workloads {
        Some(average_cu(spec, run_authorize_case)?)
    } else {
        None
    };
    let authorize_missing_signature_cu = if spec.supports_validation_workloads {
        Some(average_cu(spec, run_authorize_missing_signature_case)?)
    } else {
        None
    };
    let counter_access_cu = if spec.supports_validation_workloads {
        Some(average_cu(spec, run_counter_access_case)?)
    } else {
        None
    };
    let deposit_cu = average_cu(spec, run_deposit_case)?;
    let withdraw_cu = average_cu(spec, run_withdraw_case)?;
    let unsigned_withdraw_rejected = run_unsigned_withdraw_rejection(spec, shared_user(0))?;
    let binary_size_bytes = fs::metadata(&spec.binary_path)
        .map_err(|err| format!("failed to stat {}: {err}", spec.binary_path.display()))?
        .len();

    Ok(FrameworkMetric {
        framework: spec.framework,
        program_id: spec.program_id.to_string(),
        authorize_cu,
        authorize_missing_signature_cu,
        counter_access_cu,
        deposit_cu,
        withdraw_cu,
        authorize_vs_hopper: None,
        counter_access_vs_hopper: None,
        deposit_vs_hopper: 0,
        withdraw_vs_hopper: 0,
        binary_size_bytes,
        binary_size_kib: binary_size_bytes as f64 / 1024.0,
        unsigned_withdraw_rejected,
    })
}

fn average_cu(
    spec: &ProgramSpec,
    measure: fn(&ProgramSpec, Address) -> Result<u64, String>,
) -> Result<u64, String> {
    let mut total = 0u64;
    let mut i = 0;
    while i < SHARED_USER_CASES.len() {
        total = total
            .checked_add(measure(spec, shared_user(i))?)
            .ok_or_else(|| format!("{} CU total overflowed during averaging", spec.framework))?;
        i += 1;
    }
    Ok(total / SHARED_USER_CASES.len() as u64)
}

fn shared_user(index: usize) -> Address {
    Address::new_from_array(SHARED_USER_CASES[index])
}

fn run_deposit_case(spec: &ProgramSpec, user: Address) -> Result<u64, String> {
    let mollusk = mollusk(spec)?;
    let (system_program, system_program_account) = keyed_account_for_system_program();
    let vault = vault_address(&spec.program_id, &user);

    let result = mollusk.process_instruction(
        &deposit_instruction(spec, user, true, vault, system_program, DEPOSIT_AMOUNT),
        &[
            (user, Account::new(USER_LAMPORTS, 0, &SYSTEM_PROGRAM_ID)),
            (vault, Account::new(0, 0, &spec.program_id)),
            (system_program, system_program_account),
        ],
    );

    if !result.program_result.is_ok() {
        return Err(format!(
            "{} deposit failed: {:?}",
            spec.framework, result.program_result,
        ));
    }

    let user_after = lamports_for(&result.resulting_accounts, &user)?;
    let vault_after = lamports_for(&result.resulting_accounts, &vault)?;

    if user_after != USER_LAMPORTS - DEPOSIT_AMOUNT {
        return Err(format!(
            "{} deposit user lamports mismatch: expected {}, got {}",
            spec.framework,
            USER_LAMPORTS - DEPOSIT_AMOUNT,
            user_after,
        ));
    }
    if vault_after != DEPOSIT_AMOUNT {
        return Err(format!(
            "{} deposit vault lamports mismatch: expected {}, got {}",
            spec.framework, DEPOSIT_AMOUNT, vault_after,
        ));
    }

    Ok(result.compute_units_consumed)
}

fn run_authorize_case(spec: &ProgramSpec, user: Address) -> Result<u64, String> {
    let mollusk = mollusk(spec)?;
    let vault = vault_address(&spec.program_id, &user);

    let result = mollusk.process_instruction(
        &authorize_instruction(spec, user, true, vault),
        &[
            (user, Account::new(USER_LAMPORTS, 0, &SYSTEM_PROGRAM_ID)),
            (
                vault,
                Account::new(WITHDRAW_VAULT_LAMPORTS, 0, &spec.program_id),
            ),
        ],
    );

    if !result.program_result.is_ok() {
        return Err(format!(
            "{} authorize failed: {:?}",
            spec.framework, result.program_result,
        ));
    }

    let user_after = lamports_for(&result.resulting_accounts, &user)?;
    let vault_after = lamports_for(&result.resulting_accounts, &vault)?;

    if user_after != USER_LAMPORTS || vault_after != WITHDRAW_VAULT_LAMPORTS {
        return Err(format!(
            "{} authorize mutated balances unexpectedly: user {}, vault {}",
            spec.framework, user_after, vault_after,
        ));
    }

    Ok(result.compute_units_consumed)
}

fn run_authorize_missing_signature_case(spec: &ProgramSpec, user: Address) -> Result<u64, String> {
    let mollusk = mollusk(spec)?;
    let vault = vault_address(&spec.program_id, &user);

    let result = mollusk.process_instruction(
        &authorize_instruction(spec, user, false, vault),
        &[
            (user, Account::new(USER_LAMPORTS, 0, &SYSTEM_PROGRAM_ID)),
            (
                vault,
                Account::new(WITHDRAW_VAULT_LAMPORTS, 0, &spec.program_id),
            ),
        ],
    );

    if result.program_result.is_ok() {
        return Err(format!(
            "{} authorize missing-signature path unexpectedly succeeded",
            spec.framework,
        ));
    }

    let user_after = lamports_for(&result.resulting_accounts, &user)?;
    let vault_after = lamports_for(&result.resulting_accounts, &vault)?;
    if user_after != USER_LAMPORTS || vault_after != WITHDRAW_VAULT_LAMPORTS {
        return Err(format!(
            "{} authorize missing-signature path mutated balances unexpectedly: user {}, vault {}",
            spec.framework, user_after, vault_after,
        ));
    }

    Ok(result.compute_units_consumed)
}

fn run_counter_access_case(spec: &ProgramSpec, user: Address) -> Result<u64, String> {
    let mollusk = mollusk(spec)?;
    let vault = vault_address(&spec.program_id, &user);

    let result = mollusk.process_instruction(
        &counter_access_instruction(spec, user, true, vault),
        &[
            (user, Account::new(USER_LAMPORTS, 0, &SYSTEM_PROGRAM_ID)),
            (vault, counter_vault_account(spec, &spec.program_id, &user)),
        ],
    );

    if !result.program_result.is_ok() {
        return Err(format!(
            "{} counter access failed: {:?}",
            spec.framework, result.program_result,
        ));
    }

    let user_after = lamports_for(&result.resulting_accounts, &user)?;
    let vault_after = lamports_for(&result.resulting_accounts, &vault)?;
    let counter_after = counter_for(spec, &result.resulting_accounts, &vault)?;

    if user_after != USER_LAMPORTS || vault_after != WITHDRAW_VAULT_LAMPORTS {
        return Err(format!(
            "{} counter access mutated balances unexpectedly: user {}, vault {}",
            spec.framework, user_after, vault_after,
        ));
    }
    if counter_after != COUNTER_INITIAL_VALUE + 1 {
        return Err(format!(
            "{} counter access counter mismatch: expected {}, got {}",
            spec.framework,
            COUNTER_INITIAL_VALUE + 1,
            counter_after,
        ));
    }

    Ok(result.compute_units_consumed)
}

fn run_withdraw_case(spec: &ProgramSpec, user: Address) -> Result<u64, String> {
    let mollusk = mollusk(spec)?;
    let vault = vault_address(&spec.program_id, &user);

    let result = mollusk.process_instruction(
        &withdraw_instruction(spec, user, true, vault, WITHDRAW_AMOUNT),
        &[
            (user, Account::new(USER_LAMPORTS, 0, &SYSTEM_PROGRAM_ID)),
            (
                vault,
                Account::new(WITHDRAW_VAULT_LAMPORTS, 0, &spec.program_id),
            ),
        ],
    );

    if !result.program_result.is_ok() {
        return Err(format!(
            "{} withdraw failed: {:?}",
            spec.framework, result.program_result,
        ));
    }

    let user_after = lamports_for(&result.resulting_accounts, &user)?;
    let vault_after = lamports_for(&result.resulting_accounts, &vault)?;

    if user_after != USER_LAMPORTS + WITHDRAW_AMOUNT {
        return Err(format!(
            "{} withdraw user lamports mismatch: expected {}, got {}",
            spec.framework,
            USER_LAMPORTS + WITHDRAW_AMOUNT,
            user_after,
        ));
    }
    if vault_after != WITHDRAW_VAULT_LAMPORTS - WITHDRAW_AMOUNT {
        return Err(format!(
            "{} withdraw vault lamports mismatch: expected {}, got {}",
            spec.framework,
            WITHDRAW_VAULT_LAMPORTS - WITHDRAW_AMOUNT,
            vault_after,
        ));
    }

    Ok(result.compute_units_consumed)
}

fn run_unsigned_withdraw_rejection(spec: &ProgramSpec, user: Address) -> Result<bool, String> {
    let mollusk = mollusk(spec)?;
    let vault = vault_address(&spec.program_id, &user);

    let result = mollusk.process_instruction(
        &withdraw_instruction(spec, user, false, vault, WITHDRAW_AMOUNT),
        &[
            (user, Account::new(USER_LAMPORTS, 0, &SYSTEM_PROGRAM_ID)),
            (
                vault,
                Account::new(WITHDRAW_VAULT_LAMPORTS, 0, &spec.program_id),
            ),
        ],
    );

    if result.program_result.is_ok() {
        return Ok(false);
    }

    let user_after = lamports_for(&result.resulting_accounts, &user)?;
    let vault_after = lamports_for(&result.resulting_accounts, &vault)?;

    Ok(user_after == USER_LAMPORTS && vault_after == WITHDRAW_VAULT_LAMPORTS)
}

fn mollusk(spec: &ProgramSpec) -> Result<Mollusk, String> {
    Ok(Mollusk::new(
        &spec.program_id,
        &mollusk_program_path(&spec.binary_path),
    ))
}

fn mollusk_program_path(path: &Path) -> String {
    let mut stem = path.to_path_buf();
    stem.set_extension("");
    stem.display().to_string()
}

fn vault_address(program_id: &Address, user: &Address) -> Address {
    let (vault, _) = Address::find_program_address(&[b"vault", user.as_ref()], program_id);
    vault
}

fn is_anchor(spec: &ProgramSpec) -> bool {
    spec.framework == "anchor"
}

fn instruction_data(spec: &ProgramSpec, discriminator: u8) -> Vec<u8> {
    if is_anchor(spec) {
        let mut data = vec![0u8; 8];
        data[0] = discriminator;
        data
    } else {
        vec![discriminator]
    }
}

fn deposit_instruction(
    spec: &ProgramSpec,
    user: Address,
    user_is_signer: bool,
    vault: Address,
    system_program: Address,
    amount: u64,
) -> Instruction {
    let mut data = instruction_data(spec, 0);
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: spec.program_id,
        accounts: vec![
            AccountMeta::new(user, user_is_signer),
            AccountMeta::new(vault, false),
            AccountMeta::new_readonly(system_program, false),
        ],
        data,
    }
}

fn authorize_instruction(
    spec: &ProgramSpec,
    user: Address,
    user_is_signer: bool,
    vault: Address,
) -> Instruction {
    Instruction {
        program_id: spec.program_id,
        accounts: vec![
            AccountMeta::new(user, user_is_signer),
            AccountMeta::new(vault, false),
        ],
        data: instruction_data(spec, 2),
    }
}

fn counter_access_instruction(
    spec: &ProgramSpec,
    user: Address,
    user_is_signer: bool,
    vault: Address,
) -> Instruction {
    Instruction {
        program_id: spec.program_id,
        accounts: vec![
            AccountMeta::new(user, user_is_signer),
            AccountMeta::new(vault, false),
        ],
        data: instruction_data(spec, 3),
    }
}

fn withdraw_instruction(
    spec: &ProgramSpec,
    user: Address,
    user_is_signer: bool,
    vault: Address,
    amount: u64,
) -> Instruction {
    let mut data = instruction_data(spec, 1);
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: spec.program_id,
        accounts: vec![
            AccountMeta::new(user, user_is_signer),
            AccountMeta::new(vault, false),
        ],
        data,
    }
}

fn lamports_for(accounts: &[(Address, Account)], key: &Address) -> Result<u64, String> {
    accounts
        .iter()
        .find(|(address, _)| address == key)
        .map(|(_, account)| account.lamports)
        .ok_or_else(|| format!("missing resulting account {}", key))
}

fn counter_vault_account(spec: &ProgramSpec, program_id: &Address, authority: &Address) -> Account {
    let mut account = Account::new(WITHDRAW_VAULT_LAMPORTS, counter_data_len(spec), program_id);
    let authority_offset = counter_authority_offset(spec);
    let counter_offset = counter_offset(spec);
    if is_anchor(spec) {
        account.data[..8].copy_from_slice(&ANCHOR_COUNTER_STATE_DISCRIMINATOR);
    }
    account.data[authority_offset..authority_offset + 32].copy_from_slice(authority.as_ref());
    account.data[counter_offset..counter_offset + 8]
        .copy_from_slice(&COUNTER_INITIAL_VALUE.to_le_bytes());
    account
}

fn counter_for(
    spec: &ProgramSpec,
    accounts: &[(Address, Account)],
    key: &Address,
) -> Result<u64, String> {
    let account = accounts
        .iter()
        .find(|(address, _)| address == key)
        .map(|(_, account)| account)
        .ok_or_else(|| format!("missing resulting account {}", key))?;

    if account.data.len() < counter_data_len(spec) {
        return Err(format!(
            "resulting account {} data too small for counter access scenario",
            key,
        ));
    }

    let counter_offset = counter_offset(spec);
    let mut counter_bytes = [0u8; 8];
    counter_bytes.copy_from_slice(&account.data[counter_offset..counter_offset + 8]);
    Ok(u64::from_le_bytes(counter_bytes))
}

fn counter_data_len(spec: &ProgramSpec) -> usize {
    if is_anchor(spec) {
        ANCHOR_COUNTER_DATA_LEN
    } else {
        COUNTER_DATA_LEN
    }
}

fn counter_authority_offset(spec: &ProgramSpec) -> usize {
    if is_anchor(spec) {
        8
    } else {
        0
    }
}

fn counter_offset(spec: &ProgramSpec) -> usize {
    counter_authority_offset(spec) + 32
}

fn csv_report(report: &Report) -> String {
    let mut out = String::from(
        "Framework,ProgramId,AuthorizeCu,AuthorizeMissingSignatureCu,CounterAccessCu,DepositCu,WithdrawCu,AuthorizeVsHopper,CounterAccessVsHopper,DepositVsHopper,WithdrawVsHopper,BinarySizeBytes,BinarySizeKiB,UnsignedWithdrawRejected\n",
    );

    for benchmark in &report.benchmarks {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{:.2},{}\n",
            benchmark.framework,
            benchmark.program_id,
            csv_opt_u64(benchmark.authorize_cu),
            csv_opt_u64(benchmark.authorize_missing_signature_cu),
            csv_opt_u64(benchmark.counter_access_cu),
            benchmark.deposit_cu,
            benchmark.withdraw_cu,
            csv_opt_i64(benchmark.authorize_vs_hopper),
            csv_opt_i64(benchmark.counter_access_vs_hopper),
            benchmark.deposit_vs_hopper,
            benchmark.withdraw_vs_hopper,
            benchmark.binary_size_bytes,
            benchmark.binary_size_kib,
            benchmark.unsigned_withdraw_rejected,
        ));
    }

    out
}

fn fmt_opt_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "n/a".to_string(), |value| value.to_string())
}

fn csv_opt_u64(value: Option<u64>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn csv_opt_i64(value: Option<i64>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}
