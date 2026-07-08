//! # Quasar Parity Router
//!
//! Idiomatic Blueshift Quasar implementation of the router parity
//! contract defined in [`ROUTER_CONTRACT.md`](../ROUTER_CONTRACT.md)
//! (contract v1), measured head-to-head against the Hopper and
//! Pinocchio routers by `router-bench`.
//!
//! One instruction, `EXECUTE_ROUTE {disc=1}`, walks 1..=3 swap hops
//! against the shared `mock-amm` CPI target. Each hop's output is
//! *measured* from the user's lamport delta around the CPI — never
//! trusted from the venue — and forwarded as the next hop's input.
//! After the final hop the measured total must clear the caller's
//! `min_out` gate or the route aborts with `Custom(1)`, rolling back
//! every hop. That abort is the bench's safety-gate row.
//!
//! ## Idiomatic choices
//!
//! - `#[program]` + `#[instruction(discriminator = 1)]` +
//!   `#[derive(Accounts)]`, the shape of Quasar's own vault/escrow
//!   examples. The generated dispatch rejects unknown discriminators
//!   with `InvalidInstructionData`, per contract.
//! - `CtxWithRemaining<ExecuteRoute>`: the per-hop
//!   `[mock_amm_program, pool_i]` pairs are a variable-length tail, so
//!   they flow through Quasar's remaining-accounts iterator (which also
//!   resolves the duplicate mock-amm metas on multi-hop routes). Extra
//!   trailing accounts are ignored, per contract.
//! - The handler declares no `#[instruction]` args and parses
//!   `ctx.data` by hand: the contract's wire format carries a
//!   variable-length per-hop rate tail with **no length prefix**
//!   (`hop_count` is the counter), which Quasar's fixed/compact arg
//!   codecs cannot express byte-identically (compact `Vec` args add a
//!   length prefix). Contract fidelity wins.
//! - `CpiCall` (Quasar's const-generic stack CPI builder, the same type
//!   its own `SystemProgram::transfer` helper returns) for the 2-account
//!   17-byte hop CPI.
//! - `#[error_code]` with a pinned discriminant maps the min-out abort
//!   to exactly `Custom(1)`.
//! - `user` is an `UncheckedAccount` and writability is enforced in the
//!   handler: contract v1 pins `InvalidAccountData` for a non-writable
//!   account, while Quasar's `#[account(mut)]` constraint maps to
//!   `ProgramError::Immutable`.
//! - Checked math everywhere; no allocator (Quasar's `no_alloc!` is
//!   emitted by `#[program]` without the `alloc` feature); the router
//!   itself mutates nothing — all lamport movement happens inside
//!   mock-amm.
//!
//! ## Known contract-ordering edge (not observable in the bench rows)
//!
//! Quasar's generated dispatch checks the declared account count
//! (here: 1) *before* the handler parses instruction data, so a
//! transaction with zero accounts and malformed data returns
//! `NotEnoughAccountKeys` where the hand-rolled routers return
//! `InvalidInstructionData`. Every combination the contract's
//! measurement rows exercise is unaffected.

#![no_std]
#![allow(dead_code)]

use quasar_lang::{
    cpi::{CpiCall, InstructionAccount},
    prelude::*,
    remaining::RemainingIter,
};

#[cfg(test)]
extern crate std;

// Fixed program id, recorded in ROUTER_CONTRACT.md: `[0x51; 32]`
// (0x51 = ASCII 'Q'). Base58 of that byte array:
declare_id!("6URwbPipuA4MJLG7LCRRZuWnms3JZ9cRG3z9indXWz8G");

/// Fixed mock-amm program id (contract v1). Baked in so a route can
/// never be pointed at an arbitrary venue.
pub const MOCK_AMM_PROGRAM_ID: Address = Address::new_from_array([0xAA; 32]);

/// `EXECUTE_ROUTE` discriminator.
pub const EXECUTE_ROUTE_DISCRIMINATOR: u8 = 1;

/// Maximum hops per route (contract v1).
pub const MAX_HOPS: usize = 3;

/// Route payload header length after the discriminator byte:
/// `[min_out u64][hop_count u8][initial_amount u64]`.
pub const ROUTE_HEADER_LEN: usize = 17;

/// Per-hop rate block length: `[rate_num u32][rate_den u32]`.
pub const HOP_RATE_LEN: usize = 8;

/// Custom error code: the measured route output fell short of
/// `min_out`. Contract v1 pins this to `Custom(1)`.
pub const MIN_OUT_NOT_MET: u32 = RouterError::MinOutNotMet as u32;

/// Router errors. The discriminant is pinned so the generated
/// `From<RouterError> for ProgramError` yields exactly `Custom(1)`,
/// the contract's `MIN_OUT_NOT_MET` symbol.
#[error_code]
pub enum RouterError {
    /// The measured route output fell short of `min_out`.
    MinOutNotMet = 1,
}

const _: () = assert!(MIN_OUT_NOT_MET == 1, "contract v1 pins Custom(1)");

#[program]
mod quasar_router {
    use super::*;

    /// Execute a 1..=3 hop route against mock-amm (contract v1).
    ///
    /// No `#[instruction]` args: the wire tail is a prefix-less
    /// variable-length rate block counted by `hop_count`, parsed by
    /// hand from `ctx.data` for byte-identical contract fidelity.
    #[instruction(discriminator = 1)]
    pub fn execute_route(ctx: CtxWithRemaining<ExecuteRoute>) -> Result<(), ProgramError> {
        let route = parse_route(ctx.data)?;
        ctx.accounts.execute(&route, ctx.remaining_accounts())
    }
}

/// Declared accounts for `EXECUTE_ROUTE`: just the user; the per-hop
/// `[mock_amm_program, pool_i]` pairs arrive as remaining accounts.
#[derive(Accounts)]
pub struct ExecuteRoute {
    /// User lamport account (writable, signer not required).
    /// Deliberately unchecked here: contract v1 requires
    /// `InvalidAccountData` for a non-writable user, so the handler
    /// enforces writability itself instead of `#[account(mut)]`
    /// (which would map to `ProgramError::Immutable`).
    pub user: UncheckedAccount,
}

impl ExecuteRoute {
    /// Execute the route per contract v1.
    ///
    /// Remaining accounts, per hop `i`:
    /// `[mock_amm_program (readonly), pool_i (writable)]`. The hop
    /// output is measured from the user's lamport delta around each
    /// CPI — never trusted from the venue — and forwarded as the next
    /// hop's input. After the final hop, `total_out < min_out` aborts
    /// with `Custom(MIN_OUT_NOT_MET)`, rolling back every hop.
    #[inline(always)]
    pub fn execute(
        &self,
        route: &Route<'_>,
        remaining: RemainingAccounts<'_>,
    ) -> Result<(), ProgramError> {
        let user = self.user.to_account_view();
        require!(user.is_writable(), ProgramError::InvalidAccountData);

        let mut hop_accounts = remaining.iter();
        let mut in_amount = route.initial_amount;
        let mut hop = 0usize;
        while hop < route.hop_count as usize {
            let amm_program = next_hop_account(&mut hop_accounts)?;
            let pool = next_hop_account(&mut hop_accounts)?;

            require_keys_eq!(
                *amm_program.address(),
                MOCK_AMM_PROGRAM_ID,
                ProgramError::IncorrectProgramId
            );
            require!(pool.is_writable(), ProgramError::InvalidAccountData);

            // SAFETY: The raw view is used only to read header fields
            // (address) and as the CPI pass-through account, where the
            // runtime enforces its own borrow/aliasing rules. No account
            // data borrow is created or held here, so even a pool entry
            // aliasing the declared user (or another remaining entry)
            // cannot violate a borrow invariant.
            let pool_view = unsafe { pool.as_account_view_unchecked() };

            let (rate_num, rate_den) = route.hop_rate(hop);
            let before = user.lamports();

            CpiCall::new(
                &MOCK_AMM_PROGRAM_ID,
                [
                    InstructionAccount::writable(pool_view.address()),
                    InstructionAccount::writable(user.address()),
                ],
                [pool_view, user],
                encode_swap_data(rate_num, rate_den, in_amount),
            )
            .invoke()?;

            // Measure the hop output: out = after + in - before, checked.
            let after = user.lamports();
            in_amount = after
                .checked_add(in_amount)
                .ok_or(ProgramError::ArithmeticOverflow)?
                .checked_sub(before)
                .ok_or(ProgramError::ArithmeticOverflow)?;

            hop += 1;
        }

        require!(in_amount >= route.min_out, RouterError::MinOutNotMet);
        Ok(())
    }
}

/// Pull the next per-hop account off the remaining-accounts iterator;
/// an exhausted tail is the contract's `NotEnoughAccountKeys`.
#[inline(always)]
fn next_hop_account(hops: &mut RemainingIter<'_>) -> Result<RemainingAccount, ProgramError> {
    hops.next().ok_or(ProgramError::NotEnoughAccountKeys)?
}

/// Decoded `EXECUTE_ROUTE` payload (post-discriminator). `rates` holds
/// the raw per-hop rate blocks, length-validated against `hop_count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route<'a> {
    /// Minimum acceptable final output, in lamports.
    pub min_out: u64,
    /// Number of hops, `1..=MAX_HOPS`.
    pub hop_count: u8,
    /// Lamports fed into hop 0.
    pub initial_amount: u64,
    /// `hop_count` consecutive `[rate_num u32 LE][rate_den u32 LE]`
    /// blocks.
    pub rates: &'a [u8],
}

impl Route<'_> {
    /// The `(rate_num, rate_den)` pair for hop `i`. Caller guarantees
    /// `i < hop_count`; [`parse_route`] guarantees the backing bytes.
    #[inline(always)]
    pub fn hop_rate(&self, i: usize) -> (u32, u32) {
        let base = i * HOP_RATE_LEN;
        let num = u32::from_le_bytes([
            self.rates[base],
            self.rates[base + 1],
            self.rates[base + 2],
            self.rates[base + 3],
        ]);
        let den = u32::from_le_bytes([
            self.rates[base + 4],
            self.rates[base + 5],
            self.rates[base + 6],
            self.rates[base + 7],
        ]);
        (num, den)
    }
}

/// Parse the `EXECUTE_ROUTE` payload (the instruction data *after* the
/// discriminator byte). Contract v1 requires the total length to be
/// exactly `ROUTE_HEADER_LEN + hop_count * HOP_RATE_LEN` with
/// `hop_count` in `1..=MAX_HOPS`; anything else is
/// `InvalidInstructionData`.
#[inline(always)]
pub fn parse_route(data: &[u8]) -> Result<Route<'_>, ProgramError> {
    if data.len() < ROUTE_HEADER_LEN {
        return Err(ProgramError::InvalidInstructionData);
    }
    let min_out = u64::from_le_bytes([
        data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
    ]);
    let hop_count = data[8];
    if hop_count == 0 || hop_count as usize > MAX_HOPS {
        return Err(ProgramError::InvalidInstructionData);
    }
    let initial_amount = u64::from_le_bytes([
        data[9], data[10], data[11], data[12], data[13], data[14], data[15], data[16],
    ]);
    let expected_len = ROUTE_HEADER_LEN + hop_count as usize * HOP_RATE_LEN;
    if data.len() != expected_len {
        return Err(ProgramError::InvalidInstructionData);
    }
    Ok(Route {
        min_out,
        hop_count,
        initial_amount,
        rates: &data[ROUTE_HEADER_LEN..],
    })
}

/// Encode a mock-amm SWAP instruction:
/// `[disc=0][rate_num u32 LE][rate_den u32 LE][amount u64 LE]`.
#[inline(always)]
pub fn encode_swap_data(rate_num: u32, rate_den: u32, amount: u64) -> [u8; 17] {
    let num = rate_num.to_le_bytes();
    let den = rate_den.to_le_bytes();
    let amt = amount.to_le_bytes();
    [
        0, num[0], num[1], num[2], num[3], den[0], den[1], den[2], den[3], amt[0], amt[1], amt[2],
        amt[3], amt[4], amt[5], amt[6], amt[7],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn route_data(min_out: u64, hop_count: u8, initial: u64, rates: &[(u32, u32)]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&min_out.to_le_bytes());
        data.push(hop_count);
        data.extend_from_slice(&initial.to_le_bytes());
        for (num, den) in rates {
            data.extend_from_slice(&num.to_le_bytes());
            data.extend_from_slice(&den.to_le_bytes());
        }
        data
    }

    #[test]
    fn parse_route_roundtrips_three_hops() {
        let data = route_data(2_000_000_000, 3, 1_000_000_000, &[(3, 2), (2, 3), (2, 1)]);
        let route = parse_route(&data).unwrap();
        assert_eq!(route.min_out, 2_000_000_000);
        assert_eq!(route.hop_count, 3);
        assert_eq!(route.initial_amount, 1_000_000_000);
        assert_eq!(route.hop_rate(0), (3, 2));
        assert_eq!(route.hop_rate(1), (2, 3));
        assert_eq!(route.hop_rate(2), (2, 1));
    }

    #[test]
    fn parse_route_rejects_zero_and_excess_hop_counts() {
        let zero = route_data(1, 0, 1, &[]);
        assert_eq!(
            parse_route(&zero),
            Err(ProgramError::InvalidInstructionData)
        );
        let four = route_data(1, 4, 1, &[(1, 1), (1, 1), (1, 1), (1, 1)]);
        assert_eq!(
            parse_route(&four),
            Err(ProgramError::InvalidInstructionData)
        );
    }

    #[test]
    fn parse_route_rejects_length_mismatch() {
        // Declares 2 hops but carries rate blocks for 1.
        let short = route_data(1, 2, 1, &[(1, 1)]);
        assert_eq!(
            parse_route(&short),
            Err(ProgramError::InvalidInstructionData)
        );
        // Declares 1 hop but carries rate blocks for 2.
        let long = route_data(1, 1, 1, &[(1, 1), (1, 1)]);
        assert_eq!(
            parse_route(&long),
            Err(ProgramError::InvalidInstructionData)
        );
        // Truncated header.
        assert_eq!(
            parse_route(&[0u8; ROUTE_HEADER_LEN - 1]),
            Err(ProgramError::InvalidInstructionData)
        );
    }

    #[test]
    fn encode_swap_data_matches_contract_layout() {
        let data = encode_swap_data(3, 2, 1_000_000_000);
        assert_eq!(data.len(), 17);
        assert_eq!(data[0], 0);
        assert_eq!(u32::from_le_bytes(data[1..5].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(data[5..9].try_into().unwrap()), 2);
        assert_eq!(
            u64::from_le_bytes(data[9..17].try_into().unwrap()),
            1_000_000_000
        );
    }

    #[test]
    fn min_out_error_is_custom_one() {
        assert_eq!(
            ProgramError::from(RouterError::MinOutNotMet),
            ProgramError::Custom(1)
        );
    }

    #[test]
    fn declared_program_id_is_the_contract_byte_array() {
        // ROUTER_CONTRACT.md fixes the quasar router id as [0x51; 32].
        assert_eq!(crate::ID, Address::new_from_array([0x51; 32]));
    }
}
