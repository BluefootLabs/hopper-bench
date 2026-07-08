//! Mollusk host runner for the router parity benchmark.
//!
//! Behavioral contract: `ROUTER_CONTRACT.md` (v1) at the workspace
//! root. Modeled on `framework-vault-bench/src/main.rs`, with one
//! structural difference: every Mollusk case registers TWO programs —
//! the router under test plus the shared `mock-amm` CPI target — via
//! `Mollusk::default()` + `add_program`, because the router rows
//! exercise cross-program invocation rather than a single program.

use {
    mollusk_svm::{program::create_program_account_loader_v3, Mollusk},
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

/// Fixed program ids per `ROUTER_CONTRACT.md` v1.
const HOPPER_PROGRAM_ID: Address = Address::new_from_array([8; 32]);
const PINOCCHIO_PROGRAM_ID: Address = Address::new_from_array([0xB2; 32]);
const QUASAR_PROGRAM_ID: Address = Address::new_from_array([0x51; 32]);
/// The mock-amm id is a contract constant baked into every router
/// binary; it is deliberately NOT overridable from the command line.
const MOCK_AMM_PROGRAM_ID: Address = Address::new_from_array([0xAA; 32]);

const EXECUTE_ROUTE_DISCRIMINATOR: u8 = 1;

const USER_LAMPORTS: u64 = 10_000_000_000;
const POOL_LAMPORTS: u64 = 100_000_000_000;
const INITIAL_AMOUNT: u64 = 1_000_000_000;

/// Same 8 deterministic user seeds as the vault bench. The router
/// contract derives no PDAs, so per-seed CU variance should be zero;
/// the averaging is kept anyway for methodology consistency.
const SHARED_USER_CASES: [[u8; 32]; 8] = [
    [0x11; 32], [0x22; 32], [0x33; 32], [0x44; 32], [0x55; 32], [0x66; 32], [0x77; 32], [0x88; 32],
];

/// One measurement row: per-hop `(rate_num, rate_den)` pairs plus the
/// min-out gate value and the contract-mandated expected final output.
struct RouteCase {
    rates: &'static [(u32, u32)],
    min_out: u64,
    expected_out: u64,
}

/// `swap_1hop` / `swap_2hop` / `swap_3hop` rows (`ROUTER_CONTRACT.md`,
/// "Measurement rows"). Success rows pin `min_out` to the exact
/// expected output so the gate is exercised at its boundary.
const SWAP_CASES: [RouteCase; 3] = [
    RouteCase {
        rates: &[(3, 2)],
        min_out: 1_500_000_000,
        expected_out: 1_500_000_000,
    },
    RouteCase {
        rates: &[(3, 2), (2, 3)],
        min_out: 1_000_000_000,
        expected_out: 1_000_000_000,
    },
    RouteCase {
        rates: &[(3, 2), (2, 3), (2, 1)],
        min_out: 2_000_000_000,
        expected_out: 2_000_000_000,
    },
];

/// The safety-gate row: a 1/2 rate produces 500_000_000 against a
/// 1_000_000_000 gate; the route MUST abort with no balance movement.
const VIOLATION_CASE: RouteCase = RouteCase {
    rates: &[(1, 2)],
    min_out: 1_000_000_000,
    expected_out: 500_000_000,
};

#[derive(Serialize)]
struct Methodology {
    runner: &'static str,
    contract: &'static str,
    samples: usize,
    swap_rows: &'static str,
    amount_forwarding: &'static str,
    safety_check: &'static str,
    cu_note: &'static str,
}

#[derive(Serialize)]
struct FrameworkMetric {
    framework: &'static str,
    program_id: String,
    swap_1hop_cu: u64,
    swap_2hop_cu: u64,
    swap_3hop_cu: u64,
    swap_1hop_vs_hopper: i64,
    swap_2hop_vs_hopper: i64,
    swap_3hop_vs_hopper: i64,
    binary_size_bytes: u64,
    binary_size_kib: f64,
    min_out_violation_rejected: bool,
}

#[derive(Serialize)]
struct Report {
    hopper_root: String,
    methodology: Methodology,
    benchmarks: Vec<FrameworkMetric>,
}

struct ProgramSpec {
    framework: &'static str,
    program_id: Address,
    binary_path: PathBuf,
    /// Shared CPI target, registered alongside the router in every
    /// Mollusk case.
    mock_amm_binary_path: PathBuf,
}

struct Args {
    /// Main Hopper framework checkout. The benchmark product lives in
    /// a separate repo, so Hopper artifacts are loaded from this path.
    hopper_root: PathBuf,
    out_dir: PathBuf,
    program_ids: ProgramIdOverrides,
}

#[derive(Default)]
struct ProgramIdOverrides {
    hopper: Option<Address>,
    pinocchio: Option<Address>,
    quasar: Option<Address>,
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

    // The mock-amm artifact is not a framework row — it is the shared
    // venue every router hops through — so a missing binary is fatal
    // rather than skippable: without it no row can run.
    let mock_amm_binary_path = workspace_root.join("target/deploy/mock_amm.so");
    ensure_file(&mock_amm_binary_path)
        .map_err(|msg| format!("{msg} (build it with `cargo build-sbf` in mock-amm/)"))?;

    // Hopper is the comparison baseline. Like the vault runner, other
    // frameworks are skipped with a log line when their artifact is
    // missing; a missing Hopper artifact is logged and then fails
    // loudly at baseline lookup.
    let mut specs: Vec<ProgramSpec> = vec![
        ProgramSpec {
            framework: "hopper",
            program_id: args.program_ids.hopper.unwrap_or(HOPPER_PROGRAM_ID),
            binary_path: args.hopper_root.join("target/deploy/hopper_router.so"),
            mock_amm_binary_path: mock_amm_binary_path.clone(),
        },
        ProgramSpec {
            framework: "pinocchio",
            program_id: args.program_ids.pinocchio.unwrap_or(PINOCCHIO_PROGRAM_ID),
            binary_path: workspace_root.join("target/deploy/pinocchio_router.so"),
            mock_amm_binary_path: mock_amm_binary_path.clone(),
        },
        ProgramSpec {
            framework: "quasar",
            program_id: args.program_ids.quasar.unwrap_or(QUASAR_PROGRAM_ID),
            binary_path: workspace_root.join("target/deploy/quasar_router.so"),
            mock_amm_binary_path: mock_amm_binary_path.clone(),
        },
    ];

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
    let hopper_1hop = benchmarks[hopper_index].swap_1hop_cu;
    let hopper_2hop = benchmarks[hopper_index].swap_2hop_cu;
    let hopper_3hop = benchmarks[hopper_index].swap_3hop_cu;

    for benchmark in &mut benchmarks {
        benchmark.swap_1hop_vs_hopper = benchmark.swap_1hop_cu as i64 - hopper_1hop as i64;
        benchmark.swap_2hop_vs_hopper = benchmark.swap_2hop_cu as i64 - hopper_2hop as i64;
        benchmark.swap_3hop_vs_hopper = benchmark.swap_3hop_cu as i64 - hopper_3hop as i64;
    }

    let report = Report {
        hopper_root: args.hopper_root.display().to_string(),
        methodology: Methodology {
            runner: "mollusk-svm shared host runner; every case registers the framework's router binary plus the shared mock-amm CPI target",
            contract: "ROUTER_CONTRACT.md v1",
            samples: SHARED_USER_CASES.len(),
            swap_rows: "average across shared deterministic user seed cases; 1..=3 mock-amm swap hops with rates per ROUTER_CONTRACT.md; min_out pinned to the exact expected output so the gate is exercised at its boundary on every success row",
            amount_forwarding: "hop i+1 input equals the router's measured user-lamport delta from hop i (never venue-reported); verified via user and per-pool balance assertions",
            safety_check: "min_out violation must fail the instruction and roll back every hop's lamport movement; a framework passing it is flagged FAILED and its row is disqualified from publication",
            cu_note: "measured CU includes one identical mock-amm invocation per hop for every framework; vs_hopper deltas therefore isolate router-side framework overhead",
        },
        benchmarks,
    };

    let json_path = args.out_dir.join("router-framework-comparison.json");
    let csv_path = args.out_dir.join("router-framework-comparison.csv");

    fs::write(
        &json_path,
        serde_json::to_string_pretty(&report)
            .map_err(|err| format!("failed to serialize JSON report: {err}"))?,
    )
    .map_err(|err| format!("failed to write {}: {err}", json_path.display()))?;

    fs::write(&csv_path, csv_report(&report))
        .map_err(|err| format!("failed to write {}: {err}", csv_path.display()))?;

    println!();
    println!("Router framework comparison");
    println!(
        "{:<16} {:<44} {:>10} {:>10} {:>10} {:>11} {:>11}",
        "Framework", "ProgramId", "Swap1Hop", "Swap2Hop", "Swap3Hop", "Binary KiB", "Safety"
    );
    for benchmark in &report.benchmarks {
        println!(
            "{:<16} {:<44} {:>10} {:>10} {:>10} {:>11.2} {:>11}",
            benchmark.framework,
            benchmark.program_id,
            benchmark.swap_1hop_cu,
            benchmark.swap_2hop_cu,
            benchmark.swap_3hop_cu,
            benchmark.binary_size_kib,
            if benchmark.min_out_violation_rejected {
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
            _ => {
                return Err(format!(
                    "unknown argument '{arg}'; expected [--hopper-root <path>] [--out-dir <path>] [--hopper-program-id <id>] [--pinocchio-program-id <id>] [--quasar-program-id <id>]"
                ));
            }
        }
    }

    let workspace_root = workspace_root()?;
    let hopper_root = hopper_root
        .map(|p| canonicalize_existing(&p))
        .transpose()?
        .unwrap_or_else(|| workspace_root.clone());
    let out_dir = match out_dir {
        Some(path) if path.is_absolute() => path,
        Some(path) => workspace_root.join(path),
        None => workspace_root.join("results/router-parity"),
    };

    Ok(Args {
        hopper_root,
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
    let swap_1hop_cu = average_cu(spec, &SWAP_CASES[0])?;
    let swap_2hop_cu = average_cu(spec, &SWAP_CASES[1])?;
    let swap_3hop_cu = average_cu(spec, &SWAP_CASES[2])?;
    let min_out_violation_rejected = run_min_out_violation(spec, 0)?;
    let binary_size_bytes = fs::metadata(&spec.binary_path)
        .map_err(|err| format!("failed to stat {}: {err}", spec.binary_path.display()))?
        .len();

    Ok(FrameworkMetric {
        framework: spec.framework,
        program_id: spec.program_id.to_string(),
        swap_1hop_cu,
        swap_2hop_cu,
        swap_3hop_cu,
        swap_1hop_vs_hopper: 0,
        swap_2hop_vs_hopper: 0,
        swap_3hop_vs_hopper: 0,
        binary_size_bytes,
        binary_size_kib: binary_size_bytes as f64 / 1024.0,
        min_out_violation_rejected,
    })
}

fn average_cu(spec: &ProgramSpec, case: &RouteCase) -> Result<u64, String> {
    let mut total = 0u64;
    let mut i = 0;
    while i < SHARED_USER_CASES.len() {
        total = total
            .checked_add(run_route_case(spec, case, i)?)
            .ok_or_else(|| format!("{} CU total overflowed during averaging", spec.framework))?;
        i += 1;
    }
    Ok(total / SHARED_USER_CASES.len() as u64)
}

fn shared_user(index: usize) -> Address {
    Address::new_from_array(SHARED_USER_CASES[index])
}

/// Deterministic per-seed, per-hop pool address. No PDA derivation in
/// contract v1; pools are plain mock-amm-owned lamport accounts.
fn pool_address(seed_index: usize, hop: usize) -> Address {
    let mut bytes = [0xC1u8 + hop as u8; 32];
    bytes[0] = SHARED_USER_CASES[seed_index][0];
    Address::new_from_array(bytes)
}

/// Per-hop `(in, out)` amounts implied by the contract's rate math
/// (`out = floor(in * num / den)` in u128, forwarded as the next in).
fn expected_hops(initial: u64, rates: &[(u32, u32)]) -> Result<Vec<(u64, u64)>, String> {
    let mut hops = Vec::with_capacity(rates.len());
    let mut amount_in = initial;
    for (num, den) in rates {
        if *den == 0 {
            return Err("route case has a zero rate denominator".to_string());
        }
        let out = (amount_in as u128) * (*num as u128) / (*den as u128);
        let out =
            u64::try_from(out).map_err(|_| "route case rate math overflowed u64".to_string())?;
        hops.push((amount_in, out));
        amount_in = out;
    }
    Ok(hops)
}

/// `EXECUTE_ROUTE` wire encoding per `ROUTER_CONTRACT.md` v1:
/// `[disc=1][min_out u64 LE][hop_count u8][initial_amount u64 LE]`
/// followed by one `[rate_num u32 LE][rate_den u32 LE]` block per hop.
fn execute_route_data(min_out: u64, initial_amount: u64, rates: &[(u32, u32)]) -> Vec<u8> {
    let mut data = Vec::with_capacity(18 + 8 * rates.len());
    data.push(EXECUTE_ROUTE_DISCRIMINATOR);
    data.extend_from_slice(&min_out.to_le_bytes());
    data.push(rates.len() as u8);
    data.extend_from_slice(&initial_amount.to_le_bytes());
    for (num, den) in rates {
        data.extend_from_slice(&num.to_le_bytes());
        data.extend_from_slice(&den.to_le_bytes());
    }
    data
}

/// Build the instruction and starting account set for one route case.
/// The user is deliberately NOT a signer (contract v1); the mock-amm
/// program account appears once per hop in the metas, mirroring how a
/// real route transaction would reference the venue.
fn route_fixture(
    spec: &ProgramSpec,
    case: &RouteCase,
    seed_index: usize,
) -> (Instruction, Vec<(Address, Account)>) {
    let user = shared_user(seed_index);

    let mut metas = Vec::with_capacity(1 + 2 * case.rates.len());
    metas.push(AccountMeta::new(user, false));

    let mut accounts: Vec<(Address, Account)> = Vec::with_capacity(2 + 2 * case.rates.len());
    accounts.push((user, Account::new(USER_LAMPORTS, 0, &MOCK_AMM_PROGRAM_ID)));
    accounts.push((
        MOCK_AMM_PROGRAM_ID,
        create_program_account_loader_v3(&MOCK_AMM_PROGRAM_ID),
    ));

    for hop in 0..case.rates.len() {
        let pool = pool_address(seed_index, hop);
        metas.push(AccountMeta::new_readonly(MOCK_AMM_PROGRAM_ID, false));
        metas.push(AccountMeta::new(pool, false));
        accounts.push((pool, Account::new(POOL_LAMPORTS, 0, &MOCK_AMM_PROGRAM_ID)));
    }

    let instruction = Instruction {
        program_id: spec.program_id,
        accounts: metas,
        data: execute_route_data(case.min_out, INITIAL_AMOUNT, case.rates),
    };

    (instruction, accounts)
}

fn run_route_case(spec: &ProgramSpec, case: &RouteCase, seed_index: usize) -> Result<u64, String> {
    let mollusk = mollusk(spec)?;
    let (instruction, accounts) = route_fixture(spec, case, seed_index);
    let result = mollusk.process_instruction(&instruction, &accounts);

    if !result.program_result.is_ok() {
        return Err(format!(
            "{} swap_{}hop failed: {:?}",
            spec.framework,
            case.rates.len(),
            result.program_result,
        ));
    }

    let hops = expected_hops(INITIAL_AMOUNT, case.rates)?;
    let final_out = hops
        .last()
        .map(|(_, out)| *out)
        .ok_or_else(|| "route case has no hops".to_string())?;
    if final_out != case.expected_out {
        return Err(format!(
            "route case table is inconsistent: computed final out {final_out}, expected {}",
            case.expected_out,
        ));
    }

    let user = shared_user(seed_index);
    let user_after = lamports_for(&result.resulting_accounts, &user)?;
    let expected_user = USER_LAMPORTS - INITIAL_AMOUNT + final_out;
    if user_after != expected_user {
        return Err(format!(
            "{} swap_{}hop user lamports mismatch: expected {expected_user}, got {user_after}",
            spec.framework,
            case.rates.len(),
        ));
    }

    for (hop, (amount_in, amount_out)) in hops.iter().enumerate() {
        let pool = pool_address(seed_index, hop);
        let pool_after = lamports_for(&result.resulting_accounts, &pool)?;
        let expected_pool = POOL_LAMPORTS + amount_in - amount_out;
        if pool_after != expected_pool {
            return Err(format!(
                "{} swap_{}hop pool {hop} lamports mismatch: expected {expected_pool}, got {pool_after}",
                spec.framework,
                case.rates.len(),
            ));
        }
    }

    Ok(result.compute_units_consumed)
}

/// The safety-gate row. Returns `true` only when the route FAILS and
/// every balance (user + all pools) is unchanged — i.e. the min-out
/// gate aborted the route and the hops rolled back.
fn run_min_out_violation(spec: &ProgramSpec, seed_index: usize) -> Result<bool, String> {
    let mollusk = mollusk(spec)?;
    let (instruction, accounts) = route_fixture(spec, &VIOLATION_CASE, seed_index);
    let result = mollusk.process_instruction(&instruction, &accounts);

    if result.program_result.is_ok() {
        return Ok(false);
    }

    let user = shared_user(seed_index);
    if lamports_for(&result.resulting_accounts, &user)? != USER_LAMPORTS {
        return Ok(false);
    }
    for hop in 0..VIOLATION_CASE.rates.len() {
        let pool = pool_address(seed_index, hop);
        if lamports_for(&result.resulting_accounts, &pool)? != POOL_LAMPORTS {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Two programs per Mollusk case: the router under test plus the
/// shared mock-amm CPI target. `Mollusk::new` only registers one, so
/// this uses `Mollusk::default()` + `add_program` (mollusk-svm 0.10.3;
/// `add_program` loads the ELF by name under the default loader).
fn mollusk(spec: &ProgramSpec) -> Result<Mollusk, String> {
    let mut mollusk = Mollusk::default();
    mollusk.add_program(&spec.program_id, &mollusk_program_path(&spec.binary_path));
    mollusk.add_program(
        &MOCK_AMM_PROGRAM_ID,
        &mollusk_program_path(&spec.mock_amm_binary_path),
    );
    Ok(mollusk)
}

fn mollusk_program_path(path: &Path) -> String {
    let mut stem = path.to_path_buf();
    stem.set_extension("");
    stem.display().to_string()
}

fn lamports_for(accounts: &[(Address, Account)], key: &Address) -> Result<u64, String> {
    accounts
        .iter()
        .find(|(address, _)| address == key)
        .map(|(_, account)| account.lamports)
        .ok_or_else(|| format!("missing resulting account {}", key))
}

fn csv_report(report: &Report) -> String {
    let mut out = String::from(
        "Framework,ProgramId,Swap1HopCu,Swap2HopCu,Swap3HopCu,Swap1HopVsHopper,Swap2HopVsHopper,Swap3HopVsHopper,BinarySizeBytes,BinarySizeKiB,MinOutViolationRejected\n",
    );

    for benchmark in &report.benchmarks {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{:.2},{}\n",
            benchmark.framework,
            benchmark.program_id,
            benchmark.swap_1hop_cu,
            benchmark.swap_2hop_cu,
            benchmark.swap_3hop_cu,
            benchmark.swap_1hop_vs_hopper,
            benchmark.swap_2hop_vs_hopper,
            benchmark.swap_3hop_vs_hopper,
            benchmark.binary_size_bytes,
            benchmark.binary_size_kib,
            benchmark.min_out_violation_rejected,
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_route_data_matches_contract_layout() {
        let data = execute_route_data(2_000_000_000, 1_000_000_000, &[(3, 2), (2, 3), (2, 1)]);
        assert_eq!(data.len(), 18 + 8 * 3);
        assert_eq!(data[0], EXECUTE_ROUTE_DISCRIMINATOR);
        assert_eq!(
            u64::from_le_bytes(data[1..9].try_into().unwrap()),
            2_000_000_000
        );
        assert_eq!(data[9], 3);
        assert_eq!(
            u64::from_le_bytes(data[10..18].try_into().unwrap()),
            1_000_000_000
        );
        for (i, (num, den)) in [(3u32, 2u32), (2, 3), (2, 1)].iter().enumerate() {
            let base = 18 + 8 * i;
            assert_eq!(
                u32::from_le_bytes(data[base..base + 4].try_into().unwrap()),
                *num
            );
            assert_eq!(
                u32::from_le_bytes(data[base + 4..base + 8].try_into().unwrap()),
                *den
            );
        }
    }

    #[test]
    fn expected_hops_forwards_amounts_per_contract_rows() {
        // swap_3hop: 1e9 -> 1.5e9 -> 1e9 -> 2e9.
        let hops = expected_hops(INITIAL_AMOUNT, SWAP_CASES[2].rates).unwrap();
        assert_eq!(
            hops,
            vec![
                (1_000_000_000, 1_500_000_000),
                (1_500_000_000, 1_000_000_000),
                (1_000_000_000, 2_000_000_000),
            ]
        );
        assert_eq!(hops.last().unwrap().1, SWAP_CASES[2].expected_out);
    }

    #[test]
    fn every_success_row_pins_min_out_to_expected_out() {
        for case in &SWAP_CASES {
            let hops = expected_hops(INITIAL_AMOUNT, case.rates).unwrap();
            assert_eq!(hops.last().unwrap().1, case.expected_out);
            assert_eq!(case.min_out, case.expected_out);
        }
    }

    #[test]
    fn violation_row_falls_short_of_the_gate() {
        let hops = expected_hops(INITIAL_AMOUNT, VIOLATION_CASE.rates).unwrap();
        assert_eq!(hops.last().unwrap().1, VIOLATION_CASE.expected_out);
        assert!(VIOLATION_CASE.expected_out < VIOLATION_CASE.min_out);
    }

    #[test]
    fn contract_program_ids_are_fixed_and_pairwise_distinct() {
        // ROUTER_CONTRACT.md v1 id table.
        let ids = [
            ("mock-amm", MOCK_AMM_PROGRAM_ID, [0xAAu8; 32]),
            ("hopper", HOPPER_PROGRAM_ID, [0x08; 32]),
            ("pinocchio", PINOCCHIO_PROGRAM_ID, [0xB2; 32]),
            ("quasar", QUASAR_PROGRAM_ID, [0x51; 32]),
        ];
        for (name, id, bytes) in &ids {
            assert_eq!(*id, Address::new_from_array(*bytes), "{name} id drifted");
        }
        for (i, (_, id, _)) in ids.iter().enumerate() {
            for (other_name, other, _) in &ids[i + 1..] {
                assert_ne!(id, other, "id collides with {other_name}");
            }
        }
    }

    #[test]
    fn pool_addresses_are_distinct_per_hop_and_never_collide_with_users() {
        for seed in 0..SHARED_USER_CASES.len() {
            let user = shared_user(seed);
            let pools: Vec<Address> = (0..3).map(|hop| pool_address(seed, hop)).collect();
            for (i, pool) in pools.iter().enumerate() {
                assert_ne!(*pool, user);
                assert_ne!(*pool, MOCK_AMM_PROGRAM_ID);
                for other in &pools[i + 1..] {
                    assert_ne!(pool, other);
                }
            }
        }
    }
}
