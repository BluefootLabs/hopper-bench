# Router Parity Contract — v1

Shared behavioral contract for the cross-framework **router benchmark**
(the multi-hop swap head-to-head). Every framework under measurement
implements the *router program contract* below with byte-identical wire
format and identical observable behavior, then executes hops against
**one shared, framework-neutral CPI target**: the `mock-amm` program in
this workspace. As with the vault bench, `n/a` beats synthesis — a
framework that cannot express this contract idiomatically gets a null
row, never an approximated one.

This document is authoritative. If an implementation and this file
disagree, the implementation is wrong. Version bumps (v2, …) are
required for any wire-format or semantics change.

Clean-room note: this bench matches the *behavior class* of public
multi-hop router benchmarks (hop execution, amount forwarding, min-out
gate) but the wire format, account layout, mock AMM, and all code here
are original. No third-party benchmark code or wire format is copied.

## Design decisions (locked for v1)

- **Lamport-based**: swaps move lamports, not SPL tokens. No token
  fixtures, no ATA derivation. A token-based v2 may follow.
- **Fixed wire format**: little-endian fixed-width fields only. No
  varints, no length prefixes, no serde.
- **Shared CPI target**: the `mock-amm` program (built once, in
  Pinocchio, from `mock-amm/` in this workspace) is the swap venue for
  *every* framework, so hop-execution CU deltas isolate router-side
  framework overhead. Measured route CU therefore *includes* an
  identical mock-amm cost per hop for every row.
- **No PDA derivation in v1**: neither program derives PDAs. Pool and
  user accounts are plain lamport-holding accounts owned by `mock-amm`.
- **Deterministic**: no clock, no rent, no randomness, no state other
  than lamport balances.

## Fixed program ids

| Program | Id (byte array) |
|---|---|
| mock-amm | `[0xAA; 32]` |
| hopper router | `[0x08; 32]` |
| pinocchio router | `[0xB2; 32]` |
| quasar router | `[0x51; 32]` |

Runner flags may override the router ids (mirroring the vault bench);
the mock-amm id is a contract constant baked into every router binary
and must not be overridden.

## mock-amm program

One instruction.

### SWAP (discriminator `0`)

Instruction data — exactly **17 bytes**:

| Offset | Size | Field | Type |
|---|---|---|---|
| 0 | 1 | discriminator | `u8` = `0` |
| 1 | 4 | `rate_num` | `u32` LE |
| 5 | 4 | `rate_den` | `u32` LE |
| 9 | 8 | `amount` | `u64` LE |

Accounts, in order:

1. `pool` — writable, **owned by mock-amm**, holds lamports.
2. `user` — writable, **owned by mock-amm** in bench fixtures. Signer
   **not required**: the router CPIs on the user's behalf and value
   moves are direct lamport arithmetic inside mock-amm. (Direct debits
   require program ownership under SVM rules, hence the mock-amm-owned
   user fixture. This is a bench simplification, not a wallet model.)

Behavior (all math checked; **no state mutation other than lamports**):

1. Validate data: length exactly 17, discriminator `0`, else
   `InvalidInstructionData`. `rate_den == 0` → `InvalidInstructionData`.
2. Validate accounts: fewer than 2 → `NotEnoughAccountKeys`; `pool` not
   owned by mock-amm → `InvalidAccountOwner`; `pool` or `user` not
   writable → `InvalidAccountData`.
3. `out = floor(amount × rate_num / rate_den)` computed in `u128`;
   `out > u64::MAX` → `ArithmeticOverflow`.
4. Debit leg: `user −= amount` (`amount > user.lamports` →
   `InsufficientFunds`), `pool += amount` (overflow →
   `ArithmeticOverflow`).
5. Payout leg: `pool −= out` (`out` exceeding the post-credit pool
   balance → `InsufficientFunds`), `user += out` (overflow →
   `ArithmeticOverflow`).
6. All five arithmetic steps are computed before any balance is
   written, so a failing swap mutates nothing.

Net effect: `user Δ = out − amount`, `pool Δ = amount − out`.

## Router program contract

Each framework ships one program implementing exactly this. The Hopper
implementation lives in the framework repo at `examples/hopper-router/`;
the Pinocchio baseline lives in this workspace at `pinocchio-router/`;
the Quasar implementation lives in this workspace at `quasar-router/`
(source only — it builds outside the workspace lock, see its README).

### EXECUTE_ROUTE (discriminator `1`)

Instruction data — exactly **18 + 8 × hop_count bytes**:

| Offset | Size | Field | Type |
|---|---|---|---|
| 0 | 1 | discriminator | `u8` = `1` |
| 1 | 8 | `min_out` | `u64` LE |
| 9 | 1 | `hop_count` | `u8`, `1..=3` |
| 10 | 8 | `initial_amount` | `u64` LE |
| 18 + 8i | 4 | `rate_num[i]` | `u32` LE, hop `i` |
| 22 + 8i | 4 | `rate_den[i]` | `u32` LE, hop `i` |

(The per-hop rate block is a v1 refinement: it keeps the top-level wire
fixed-width while letting the runner drive distinct rates per hop, so
multi-hop rows exercise per-hop wire parsing rather than a constant.)

Accounts, in order:

1. `user` — writable. Signer **not required**.
2. Then per hop `i` (0-based): `mock_amm_program` (readonly, the fixed
   mock-amm id), `pool_i` (writable).

So `1 + 2 × hop_count` accounts minimum; extra trailing accounts are
ignored (house pattern, same as the vault contract).

Behavior:

1. Unknown discriminator → `InvalidInstructionData`. Data shorter than
   the 18-byte header, `hop_count` outside `1..=3`, or total length ≠
   `18 + 8 × hop_count` → `InvalidInstructionData`.
2. Fewer than `1 + 2 × hop_count` accounts → `NotEnoughAccountKeys`.
   `user` not writable → `InvalidAccountData`.
3. `in_0 = initial_amount`. For each hop `i` in order:
   - hop program account address ≠ mock-amm id → `IncorrectProgramId`;
     `pool_i` not writable → `InvalidAccountData`.
   - `before = user.lamports`.
   - CPI mock-amm `SWAP { rate_num[i], rate_den[i], amount: in_i }`
     with accounts `[pool_i (writable), user (writable)]`, no signer
     seeds. CPI failure propagates and aborts the route.
   - `after = user.lamports`. The hop output is **measured, not
     trusted**: `out_i = after + in_i − before`, every step checked
     (`ArithmeticOverflow` on any wrap). With the v1 mock-amm this
     equals `floor(in_i × rate_num[i] / rate_den[i])`, but routers must
     derive it from the observed user-lamport delta.
   - Forward the amount: `in_{i+1} = out_i`.
4. Min-out gate, after the final hop: `total_out = out_{hop_count−1}`.
   `total_out < min_out` → **custom error `Custom(1)`** (symbol
   `MIN_OUT_NOT_MET`). The route MUST abort — instruction failure rolls
   back every hop's lamport movement. This is the safety-gate row.
5. On success the program returns `Ok(())` having mutated nothing
   directly itself (all lamport movement happens inside mock-amm).

## Measurement rows

Per framework, mirroring the vault bench methodology (mollusk-svm
0.10.3 host runner, CU averaged over the same 8 deterministic user
seeds `[0x11;32] … [0x88;32]`, binary size and — at integration time —
max stack frame recorded from the SBF build):

| Row | hop_count | rates (num/den per hop) | initial | min_out | expected `total_out` |
|---|---|---|---|---|---|
| `swap_1hop` | 1 | 3/2 | 1_000_000_000 | 1_500_000_000 | 1_500_000_000 |
| `swap_2hop` | 2 | 3/2, 2/3 | 1_000_000_000 | 1_000_000_000 | 1_000_000_000 |
| `swap_3hop` | 3 | 3/2, 2/3, 2/1 | 1_000_000_000 | 2_000_000_000 | 2_000_000_000 |
| `min_out_violation_rejected` | 1 | 1/2 | 1_000_000_000 | 1_000_000_000 | must FAIL (`out` = 500_000_000) |

Fixtures per case: `user` = seed address, `10_000_000_000` lamports,
0 data, owned by mock-amm. Each `pool_i` = deterministic per-seed
address, `100_000_000_000` lamports, 0 data, owned by mock-amm. The
mock-amm program account is registered with the runner's Mollusk
instance alongside the router under test (two programs per case) and
included in the instruction account list.

Success rows verify, in addition to CU:

- `user` final = `10_000_000_000 − initial + total_out`;
- each `pool_i` Δ = `in_i − out_i`.

The safety-gate row (`min_out_violation_rejected`) verifies the
instruction FAILS and that `user` and every pool balance are unchanged
(rollback). Handling mirrors the vault bench's
`unsigned_withdraw_rejected`: the result is a boolean column; a
framework whose router *passes* the violating route is flagged
(`FAILED` in the table) and its row is disqualified from publication.

Note on min-out equality: `total_out >= min_out` passes, so the success
rows pin `min_out` to the exact expected output — the gate is exercised
at its boundary on every success row, and the violation row exercises
the reject branch.

## Artifacts

- mock-amm: `<bench workspace>/target/deploy/mock_amm.so` (required —
  the runner errors out without it).
- pinocchio router: `<bench workspace>/target/deploy/pinocchio_router.so`
  (missing → row skipped with a log line).
- quasar router: `<bench workspace>/target/deploy/quasar_router.so`
  (missing → row skipped with a log line; built from `quasar-router/`
  with its own lockfile and copied here — see `quasar-router/README.md`).
- hopper router: `<hopper_root>/target/deploy/hopper_router.so`
  (missing → logged; Hopper is the comparison baseline so the run then
  fails at baseline lookup, like the vault runner).

SBF builds and CU measurement happen at the integration phase
(`cargo build-sbf`); host `cargo build` / `cargo test` cover wire
codecs and pure math.
