//! R3 harness: eager vs lazy entrypoint CU comparison for the
//! `lazy-dispatch-vault` bench target.
//!
//! This is a **Hopper-vs-Hopper lab** — one framework, two entrypoint
//! strategies (`fast_entrypoint!` pre-parsing all accounts vs
//! `hopper_lazy_entrypoint!` parsing on demand). It is NOT a
//! cross-framework comparison; see `framework-vault-bench` for that.
//!
//! The harness builds nothing itself. Produce the two artifacts first
//! (from the hopper-bench workspace root):
//!
//! ```text
//! cargo build-sbf --manifest-path lazy-dispatch-vault/Cargo.toml --no-default-features --features eager
//! #   then copy target/deploy/lazy_dispatch_vault.so -> target/deploy/lazy_dispatch_vault_eager.so
//! cargo build-sbf --manifest-path lazy-dispatch-vault/Cargo.toml --no-default-features --features lazy
//! #   then copy target/deploy/lazy_dispatch_vault.so -> target/deploy/lazy_dispatch_vault_lazy.so
//! ```
//!
//! (`--help` prints copy-pasteable PowerShell and bash forms.)
//!
//! Every one of the vault's eight instructions runs against BOTH .so
//! files with byte-identical fixtures: the same eight accounts are
//! passed every time, and each instruction touches only its declared
//! subset. That is the design point — the eager entrypoint pays to
//! parse all eight accounts before dispatch, the lazy entrypoint only
//! materialises what the variant asks for.
//!
//! CU semantics: WHOLE-instruction (`compute_units_consumed` reported
//! by Mollusk), single run per cell — Mollusk is deterministic for
//! fixed fixtures. Artifacts must be debug=0 release builds (house
//! rule: never measure with `debug = 2` in the release profile).

use {
    mollusk_svm::{program::keyed_account_for_system_program, Mollusk},
    solana_account::Account,
    solana_address::Address,
    solana_instruction::{AccountMeta, Instruction},
    std::{
        env, fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    },
};

/// Deterministic program id for both variants (loaded into separate
/// Mollusk instances, so the shared id keeps fixtures byte-identical).
const PROGRAM_ID: Address = Address::new_from_array([0x1d; 32]);
const SYSTEM_PROGRAM_ID: Address = Address::new_from_array([0; 32]);

const USER_ADDRESS: Address = Address::new_from_array([0x11; 32]);
const VAULT_ADDRESS: Address = Address::new_from_array([0x22; 32]);
const FILLER_ADDRESSES: [Address; 5] = [
    Address::new_from_array([0x41; 32]),
    Address::new_from_array([0x42; 32]),
    Address::new_from_array([0x43; 32]),
    Address::new_from_array([0x44; 32]),
    Address::new_from_array([0x45; 32]),
];

const USER_LAMPORTS: u64 = 10_000_000_000;
const VAULT_LAMPORTS: u64 = 1_000_000_000;
const FILLER_LAMPORTS: u64 = 1_000_000;
/// The vault's counter instruction projects a `WireU64` at offset 0.
const VAULT_DATA_LEN: usize = 8;
const COUNTER_INITIAL_VALUE: u64 = 7;

/// Instruction map, mirrored from `lazy-dispatch-vault/src/lib.rs`.
const CASES: [(u8, &str, &str); 8] = [
    (0, "ping", "0/8"),
    (1, "get_balance", "1/8"),
    (2, "authorize", "2/8"),
    (3, "counter", "2/8"),
    (4, "deposit", "3/8"),
    (5, "withdraw", "2/8"),
    (6, "sweep", "8/8"),
    (7, "flush", "8/8"),
];

const BUILD_CMD_EAGER: &str = "cargo build-sbf --manifest-path lazy-dispatch-vault/Cargo.toml --no-default-features --features eager";
const BUILD_CMD_LAZY: &str = "cargo build-sbf --manifest-path lazy-dispatch-vault/Cargo.toml --no-default-features --features lazy";

const HELP: &str = "\
lazy-dispatch-bench: eager vs lazy Hopper entrypoint CU comparison (R3)

Hopper-vs-Hopper lab: one framework, two entrypoint strategies. Runs all
eight lazy-dispatch-vault instructions against both .so builds with
identical fixtures and prints a CU table (eager | lazy | delta) plus
binary sizes.

USAGE:
    cargo run -p lazy-dispatch-bench -- [--eager-so <path>] [--lazy-so <path>]

Defaults discover <workspace>/target/deploy/lazy_dispatch_vault_eager.so
and lazy_dispatch_vault_lazy.so.

The harness builds nothing itself. Produce the artifacts first, from the
hopper-bench workspace root (debug=0 release builds; NEVER measure with
`debug = 2` in the release profile):

PowerShell:
    cargo build-sbf --manifest-path lazy-dispatch-vault/Cargo.toml --no-default-features --features eager
    Copy-Item target\\deploy\\lazy_dispatch_vault.so target\\deploy\\lazy_dispatch_vault_eager.so -Force
    cargo build-sbf --manifest-path lazy-dispatch-vault/Cargo.toml --no-default-features --features lazy
    Copy-Item target\\deploy\\lazy_dispatch_vault.so target\\deploy\\lazy_dispatch_vault_lazy.so -Force

bash:
    cargo build-sbf --manifest-path lazy-dispatch-vault/Cargo.toml --no-default-features --features eager
    cp target/deploy/lazy_dispatch_vault.so target/deploy/lazy_dispatch_vault_eager.so
    cargo build-sbf --manifest-path lazy-dispatch-vault/Cargo.toml --no-default-features --features lazy
    cp target/deploy/lazy_dispatch_vault.so target/deploy/lazy_dispatch_vault_lazy.so
";

struct Args {
    eager_so: PathBuf,
    lazy_so: PathBuf,
}

struct Row {
    disc: u8,
    name: &'static str,
    touched: &'static str,
    eager_cu: u64,
    lazy_cu: u64,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    ensure_file(&args.eager_so)?;
    ensure_file(&args.lazy_so)?;

    let eager_size = file_size(&args.eager_so)?;
    let lazy_size = file_size(&args.lazy_so)?;

    println!();
    println!("Lazy-dispatch vault: eager vs lazy entrypoint (Hopper-vs-Hopper)");
    println!("=================================================================");
    println!("Date (UTC):     {}", utc_timestamp());
    println!("Runner:         mollusk-svm {}", mollusk_version());
    println!("Program id:     {PROGRAM_ID}");
    println!(
        "Eager .so:      {} ({} B / {:.2} KiB)",
        args.eager_so.display(),
        fmt_u64(eager_size),
        eager_size as f64 / 1024.0
    );
    println!(
        "Lazy .so:       {} ({} B / {:.2} KiB)",
        args.lazy_so.display(),
        fmt_u64(lazy_size),
        lazy_size as f64 / 1024.0
    );
    println!("Build (eager):  {BUILD_CMD_EAGER}");
    println!("Build (lazy):   {BUILD_CMD_LAZY}");
    println!(
        "CU semantics:   WHOLE-instruction (Mollusk compute_units_consumed); \
         single run per cell (deterministic fixtures); debug=0 artifacts"
    );
    println!(
        "Fixtures:       identical for every instruction and both variants — \
         8 accounts (user signer+writable, vault writable 8B data, \
         system program, 5 read-only fillers), data = [disc]"
    );
    println!("Comparison:     one framework, two entrypoint strategies — NOT cross-framework");
    println!();

    let mut rows = Vec::with_capacity(CASES.len());
    for (disc, name, touched) in CASES {
        let eager_cu = run_case(&args.eager_so, "eager", disc, name)?;
        let lazy_cu = run_case(&args.lazy_so, "lazy", disc, name)?;
        rows.push(Row {
            disc,
            name,
            touched,
            eager_cu,
            lazy_cu,
        });
    }

    println!(
        "{:<5} {:<13} {:<8} {:>9} {:>9} {:>7} {:>9}",
        "Disc", "Instruction", "Touched", "Eager CU", "Lazy CU", "Delta", "Delta%"
    );
    for row in &rows {
        let delta = row.lazy_cu as i64 - row.eager_cu as i64;
        let delta_pct = delta as f64 / row.eager_cu as f64 * 100.0;
        println!(
            "{:<5} {:<13} {:<8} {:>9} {:>9} {:>+7} {:>+8.1}%",
            row.disc, row.name, row.touched, row.eager_cu, row.lazy_cu, delta, delta_pct
        );
    }
    println!();
    println!(
        "Binary size: eager {} B ({:.2} KiB) | lazy {} B ({:.2} KiB) | delta {:+} B",
        fmt_u64(eager_size),
        eager_size as f64 / 1024.0,
        fmt_u64(lazy_size),
        lazy_size as f64 / 1024.0,
        lazy_size as i64 - eager_size as i64
    );
    println!("Delta = lazy - eager; negative = lazy is cheaper.");

    Ok(())
}

fn parse_args() -> Result<Args, String> {
    let mut eager_so = None;
    let mut lazy_so = None;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "--eager-so" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --eager-so".to_string())?;
                eager_so = Some(PathBuf::from(value));
            }
            "--lazy-so" => {
                let value = args
                    .next()
                    .ok_or_else(|| "missing value for --lazy-so".to_string())?;
                lazy_so = Some(PathBuf::from(value));
            }
            _ => {
                return Err(format!(
                    "unknown argument '{arg}'; expected [--eager-so <path>] [--lazy-so <path>] (see --help)"
                ));
            }
        }
    }

    let deploy_dir = workspace_root()?.join("target/deploy");
    Ok(Args {
        eager_so: eager_so.unwrap_or_else(|| deploy_dir.join("lazy_dispatch_vault_eager.so")),
        lazy_so: lazy_so.unwrap_or_else(|| deploy_dir.join("lazy_dispatch_vault_lazy.so")),
    })
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

fn ensure_file(path: &Path) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!(
            "missing benchmark artifact {} (build it first; see --help)",
            path.display()
        ))
    }
}

fn file_size(path: &Path) -> Result<u64, String> {
    fs::metadata(path)
        .map(|meta| meta.len())
        .map_err(|err| format!("failed to stat {}: {err}", path.display()))
}

/// Run one instruction against one artifact and return whole-instruction CU.
///
/// Both variants receive byte-identical fixtures: all eight accounts are
/// always passed; the instruction decides how many it actually touches.
fn run_case(binary: &Path, variant: &str, disc: u8, name: &str) -> Result<u64, String> {
    let mollusk = Mollusk::new(&PROGRAM_ID, &mollusk_program_path(binary));

    let (system_program, system_program_account) = keyed_account_for_system_program();

    let mut vault_account = Account::new(VAULT_LAMPORTS, VAULT_DATA_LEN, &PROGRAM_ID);
    vault_account.data[..8].copy_from_slice(&COUNTER_INITIAL_VALUE.to_le_bytes());

    let mut accounts: Vec<(Address, Account)> = vec![
        (
            USER_ADDRESS,
            Account::new(USER_LAMPORTS, 0, &SYSTEM_PROGRAM_ID),
        ),
        (VAULT_ADDRESS, vault_account),
        (system_program, system_program_account),
    ];
    for filler in FILLER_ADDRESSES {
        accounts.push((filler, Account::new(FILLER_LAMPORTS, 0, &SYSTEM_PROGRAM_ID)));
    }

    let instruction = Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(USER_ADDRESS, true),
            AccountMeta::new(VAULT_ADDRESS, false),
            AccountMeta::new_readonly(system_program, false),
            AccountMeta::new_readonly(FILLER_ADDRESSES[0], false),
            AccountMeta::new_readonly(FILLER_ADDRESSES[1], false),
            AccountMeta::new_readonly(FILLER_ADDRESSES[2], false),
            AccountMeta::new_readonly(FILLER_ADDRESSES[3], false),
            AccountMeta::new_readonly(FILLER_ADDRESSES[4], false),
        ],
        data: vec![disc],
    };

    let result = mollusk.process_instruction(&instruction, &accounts);

    if !result.program_result.is_ok() {
        return Err(format!(
            "{variant} `{name}` (disc {disc}) failed: {:?}",
            result.program_result
        ));
    }

    // No instruction in this vault moves lamports (deposit deliberately
    // skips the CPI); balances must be untouched.
    let user_after = lamports_for(&result.resulting_accounts, &USER_ADDRESS)?;
    let vault_after = lamports_for(&result.resulting_accounts, &VAULT_ADDRESS)?;
    if user_after != USER_LAMPORTS || vault_after != VAULT_LAMPORTS {
        return Err(format!(
            "{variant} `{name}` (disc {disc}) mutated balances unexpectedly: user {user_after}, vault {vault_after}"
        ));
    }

    // Only `counter` (disc 3) writes vault data; everything else must
    // leave the counter at its initial value.
    let expected_counter = if disc == 3 {
        COUNTER_INITIAL_VALUE + 1
    } else {
        COUNTER_INITIAL_VALUE
    };
    let counter_after = counter_for(&result.resulting_accounts, &VAULT_ADDRESS)?;
    if counter_after != expected_counter {
        return Err(format!(
            "{variant} `{name}` (disc {disc}) counter mismatch: expected {expected_counter}, got {counter_after}"
        ));
    }

    Ok(result.compute_units_consumed)
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
        .ok_or_else(|| format!("missing resulting account {key}"))
}

fn counter_for(accounts: &[(Address, Account)], key: &Address) -> Result<u64, String> {
    let account = accounts
        .iter()
        .find(|(address, _)| address == key)
        .map(|(_, account)| account)
        .ok_or_else(|| format!("missing resulting account {key}"))?;
    if account.data.len() < 8 {
        return Err(format!(
            "resulting account {key} data too small for counter"
        ));
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&account.data[..8]);
    Ok(u64::from_le_bytes(bytes))
}

/// Resolved mollusk-svm version, read from the workspace Cargo.lock so the
/// header reports what actually ran rather than a hardcoded string.
fn mollusk_version() -> String {
    let Ok(root) = workspace_root() else {
        return "unknown".to_string();
    };
    let Ok(lock) = fs::read_to_string(root.join("Cargo.lock")) else {
        return "unknown".to_string();
    };
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "name = \"mollusk-svm\"" {
            for candidate in lines.by_ref() {
                let candidate = candidate.trim();
                if let Some(version) = candidate
                    .strip_prefix("version = \"")
                    .and_then(|rest| rest.strip_suffix('"'))
                {
                    return version.to_string();
                }
                if candidate.is_empty() || candidate.starts_with("[[") {
                    break;
                }
            }
        }
    }
    "unknown".to_string()
}

fn fmt_u64(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn utc_timestamp() -> String {
    let Ok(elapsed) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return "unknown".to_string();
    };
    let secs = elapsed.as_secs();
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days-since-epoch to (year, month, day), Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}
