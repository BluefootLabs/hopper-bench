//! # Pinocchio Parity Router
//!
//! Idiomatic Anza Pinocchio implementation of the router parity
//! contract defined in [`ROUTER_CONTRACT.md`](../ROUTER_CONTRACT.md)
//! (contract v1).
//!
//! This is the **raw-substrate baseline** for the router benchmark,
//! modeled on `pinocchio-vault/`. One instruction, `EXECUTE_ROUTE`,
//! walks 1..=3 swap hops against the shared `mock-amm` CPI target:
//! each hop's output is *measured* from the user's lamport delta
//! (never trusted from the venue), forwarded as the next hop's input,
//! and the final output must clear the caller-supplied `min_out` gate
//! or the whole route aborts with `Custom(1)`.
//!
//! ## Idiomatic choices
//!
//! - Standard `program_entrypoint!` (eager parse), matching the vault
//!   baseline.
//! - `pinocchio::cpi::invoke::<2>` with stack `InstructionAccount`
//!   metas for the hop CPI — the validated const-generic path most
//!   Pinocchio programs ship.
//! - Checked math everywhere; no allocator; lamports are the only
//!   state touched (and only inside mock-amm).

#![cfg_attr(target_os = "solana", no_std)]
#![allow(clippy::result_large_err)]

use pinocchio::error::ProgramError;
use pinocchio::instruction::{InstructionAccount, InstructionView};
use pinocchio::{program_entrypoint, AccountView, Address, ProgramResult};

#[cfg(target_os = "solana")]
pinocchio::no_allocator!();
#[cfg(target_os = "solana")]
pinocchio::nostd_panic_handler!();

program_entrypoint!(process_instruction);

/// Fixed mock-amm program id (contract v1). Baked in so a route can
/// never be pointed at an arbitrary venue.
pub const MOCK_AMM_PROGRAM_ID: [u8; 32] = [0xAA; 32];

/// `EXECUTE_ROUTE` discriminator.
pub const EXECUTE_ROUTE_DISCRIMINATOR: u8 = 1;

/// Maximum hops per route (contract v1).
pub const MAX_HOPS: usize = 3;

/// Route payload header length after the discriminator byte:
/// `[min_out u64][hop_count u8][initial_amount u64]`.
pub const ROUTE_HEADER_LEN: usize = 17;

/// Per-hop rate block length: `[rate_num u32][rate_den u32]`.
pub const HOP_RATE_LEN: usize = 8;

/// Custom error: the measured route output fell short of `min_out`.
pub const MIN_OUT_NOT_MET: u32 = 1;

#[inline(always)]
fn process_instruction(
    _program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let (disc, rest) = instruction_data
        .split_first()
        .ok_or(ProgramError::InvalidInstructionData)?;

    match *disc {
        EXECUTE_ROUTE_DISCRIMINATOR => process_execute_route(accounts, rest),
        _ => Err(ProgramError::InvalidInstructionData),
    }
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

/// Execute the route per contract v1.
///
/// Accounts: `[user (writable)]` then per hop
/// `[mock_amm_program, pool_i (writable)]`. The hop output is measured
/// from the user's lamport delta around each CPI — never trusted from
/// the venue — and forwarded as the next hop's input. After the final
/// hop, `total_out < min_out` aborts with `Custom(MIN_OUT_NOT_MET)`,
/// rolling back every hop.
fn process_execute_route(accounts: &[AccountView], data: &[u8]) -> ProgramResult {
    let route = parse_route(data)?;
    let hops = route.hop_count as usize;

    if accounts.len() < 1 + 2 * hops {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let user = &accounts[0];
    if !user.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }

    let mut in_amount = route.initial_amount;
    let mut hop = 0;
    while hop < hops {
        let amm_program = &accounts[1 + 2 * hop];
        let pool = &accounts[2 + 2 * hop];

        if amm_program.address().to_bytes() != MOCK_AMM_PROGRAM_ID {
            return Err(ProgramError::IncorrectProgramId);
        }
        if !pool.is_writable() {
            return Err(ProgramError::InvalidAccountData);
        }

        let (rate_num, rate_den) = route.hop_rate(hop);
        let swap_data = encode_swap_data(rate_num, rate_den, in_amount);

        let before = user.lamports();

        let metas = [
            InstructionAccount::writable(pool.address()),
            InstructionAccount::writable(user.address()),
        ];
        let instruction = InstructionView {
            program_id: amm_program.address(),
            data: &swap_data,
            accounts: &metas,
        };
        pinocchio::cpi::invoke::<2>(&instruction, &[pool, user])?;

        // Measure the hop output: out = after + in - before, checked.
        let after = user.lamports();
        in_amount = after
            .checked_add(in_amount)
            .ok_or(ProgramError::ArithmeticOverflow)?
            .checked_sub(before)
            .ok_or(ProgramError::ArithmeticOverflow)?;

        hop += 1;
    }

    if in_amount < route.min_out {
        return Err(ProgramError::Custom(MIN_OUT_NOT_MET));
    }
    Ok(())
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
}
