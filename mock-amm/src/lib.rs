//! # Mock AMM
//!
//! The shared CPI target for the router parity benchmark defined in
//! [`ROUTER_CONTRACT.md`](../ROUTER_CONTRACT.md) (contract v1).
//!
//! Every framework's router implementation hops through this exact
//! binary, so router-row CU deltas isolate router-side framework
//! overhead rather than venue differences. It is deliberately built in
//! Anza Pinocchio — the raw-substrate baseline of the vault bench —
//! and models `pinocchio-vault/`'s idioms: direct lamport arithmetic,
//! checked math throughout, no allocator, no state mutation beyond
//! lamports.
//!
//! One instruction:
//!
//! - `SWAP {disc=0}[rate_num u32 LE][rate_den u32 LE][amount u64 LE]`
//!   over accounts `[pool (writable, mock-amm-owned), user (writable)]`.
//!   Debits `amount` lamports user -> pool, then pays out
//!   `floor(amount * rate_num / rate_den)` pool -> user. Signer is not
//!   required on `user`: routers CPI on the user's behalf and the bench
//!   fixtures make `user` mock-amm-owned so the direct debit is legal
//!   under SVM ownership rules.

#![cfg_attr(target_os = "solana", no_std)]
#![allow(clippy::result_large_err)]

use pinocchio::error::ProgramError;
use pinocchio::{program_entrypoint, AccountView, Address, ProgramResult};

#[cfg(target_os = "solana")]
pinocchio::no_allocator!();
#[cfg(target_os = "solana")]
pinocchio::nostd_panic_handler!();

program_entrypoint!(process_instruction);

/// Exact byte length of the SWAP instruction data (see contract v1):
/// `[disc u8][rate_num u32 LE][rate_den u32 LE][amount u64 LE]`.
pub const SWAP_DATA_LEN: usize = 17;

/// SWAP instruction discriminator.
pub const SWAP_DISCRIMINATOR: u8 = 0;

#[inline(always)]
fn process_instruction(
    program_id: &Address,
    accounts: &[AccountView],
    instruction_data: &[u8],
) -> ProgramResult {
    let args = parse_swap(instruction_data)?;
    process_swap(program_id, accounts, &args)
}

/// Decoded SWAP arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapArgs {
    /// Rate numerator.
    pub rate_num: u32,
    /// Rate denominator. Guaranteed non-zero by [`parse_swap`].
    pub rate_den: u32,
    /// Lamports the user pays into the pool.
    pub amount: u64,
}

/// Parse and validate the full SWAP instruction data (including the
/// discriminator byte). Contract v1 requires the length to be exactly
/// [`SWAP_DATA_LEN`], the discriminator to be `0`, and `rate_den` to be
/// non-zero; anything else is `InvalidInstructionData`.
#[inline(always)]
pub fn parse_swap(data: &[u8]) -> Result<SwapArgs, ProgramError> {
    if data.len() != SWAP_DATA_LEN {
        return Err(ProgramError::InvalidInstructionData);
    }
    if data[0] != SWAP_DISCRIMINATOR {
        return Err(ProgramError::InvalidInstructionData);
    }
    let rate_num = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
    let rate_den = u32::from_le_bytes([data[5], data[6], data[7], data[8]]);
    if rate_den == 0 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let amount = u64::from_le_bytes([
        data[9], data[10], data[11], data[12], data[13], data[14], data[15], data[16],
    ]);
    Ok(SwapArgs {
        rate_num,
        rate_den,
        amount,
    })
}

/// Compute the payout `floor(amount * rate_num / rate_den)` in `u128`
/// so the multiply cannot wrap; a quotient above `u64::MAX` is an
/// `ArithmeticOverflow`.
///
/// Callers must have rejected `rate_den == 0` already ([`parse_swap`]
/// does); the debug assert documents that precondition without putting
/// a branch in the release path.
#[inline(always)]
pub fn swap_out(amount: u64, rate_num: u32, rate_den: u32) -> Result<u64, ProgramError> {
    debug_assert!(rate_den != 0);
    let out = (amount as u128) * (rate_num as u128) / (rate_den as u128);
    u64::try_from(out).map_err(|_| ProgramError::ArithmeticOverflow)
}

/// Execute the swap per contract v1.
///
/// All five arithmetic results (user debit, pool credit, payout, pool
/// debit, user credit) are computed with checked math *before* any
/// balance is written, so a failing swap mutates nothing.
///
/// Accounts: `[pool (writable, owned by this program), user (writable)]`.
fn process_swap(program_id: &Address, accounts: &[AccountView], args: &SwapArgs) -> ProgramResult {
    let [pool, user, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    // Owner check on the pool. `owner()` is `unsafe` in pinocchio
    // because it aliases the BPF input buffer; the byte copy below is
    // immediate and the reference does not escape.
    let pool_owner = unsafe { owner_bytes(pool) };
    if pool_owner != program_id.to_bytes() {
        return Err(ProgramError::InvalidAccountOwner);
    }
    if !pool.is_writable() || !user.is_writable() {
        return Err(ProgramError::InvalidAccountData);
    }

    // Compute every post-swap balance before committing any of them.
    // Contract v1 step order: the payout (step 3) is computed before the
    // debit/credit legs (steps 4-5), so a payout overflow reports
    // ArithmeticOverflow even when the user also lacks funds.
    let out = swap_out(args.amount, args.rate_num, args.rate_den)?;
    let user_after_debit = user
        .lamports()
        .checked_sub(args.amount)
        .ok_or(ProgramError::InsufficientFunds)?;
    let pool_after_credit = pool
        .lamports()
        .checked_add(args.amount)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    let pool_final = pool_after_credit
        .checked_sub(out)
        .ok_or(ProgramError::InsufficientFunds)?;
    let user_final = user_after_debit
        .checked_add(out)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    pool.set_lamports(pool_final);
    user.set_lamports(user_final);
    Ok(())
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Snapshot the owner address of `view` as a value.
///
/// # Safety
///
/// Caller must ensure no concurrent mutation of `view`'s header while
/// this function executes. Callers above only use it for an immediate
/// byte-array comparison, so the reference does not escape.
#[inline(always)]
unsafe fn owner_bytes(view: &AccountView) -> [u8; 32] {
    // SAFETY: `owner()` aliases the BPF input buffer; the value is
    // copied out immediately and no reference escapes this call.
    unsafe { view.owner() }.to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swap_data(disc: u8, rate_num: u32, rate_den: u32, amount: u64) -> [u8; SWAP_DATA_LEN] {
        let mut data = [0u8; SWAP_DATA_LEN];
        data[0] = disc;
        data[1..5].copy_from_slice(&rate_num.to_le_bytes());
        data[5..9].copy_from_slice(&rate_den.to_le_bytes());
        data[9..17].copy_from_slice(&amount.to_le_bytes());
        data
    }

    #[test]
    fn parse_swap_roundtrips() {
        let data = swap_data(SWAP_DISCRIMINATOR, 3, 2, 1_000_000_000);
        assert_eq!(
            parse_swap(&data),
            Ok(SwapArgs {
                rate_num: 3,
                rate_den: 2,
                amount: 1_000_000_000,
            })
        );
    }

    #[test]
    fn parse_swap_rejects_wrong_length() {
        let data = swap_data(SWAP_DISCRIMINATOR, 1, 1, 5);
        assert_eq!(
            parse_swap(&data[..16]),
            Err(ProgramError::InvalidInstructionData)
        );
        let mut long = [0u8; SWAP_DATA_LEN + 1];
        long[..SWAP_DATA_LEN].copy_from_slice(&data);
        assert_eq!(parse_swap(&long), Err(ProgramError::InvalidInstructionData));
    }

    #[test]
    fn parse_swap_rejects_bad_discriminator() {
        let data = swap_data(1, 1, 1, 5);
        assert_eq!(parse_swap(&data), Err(ProgramError::InvalidInstructionData));
    }

    #[test]
    fn parse_swap_rejects_zero_denominator() {
        let data = swap_data(SWAP_DISCRIMINATOR, 1, 0, 5);
        assert_eq!(parse_swap(&data), Err(ProgramError::InvalidInstructionData));
    }

    #[test]
    fn swap_out_matches_contract_rows() {
        // The three success-row hops from ROUTER_CONTRACT.md.
        assert_eq!(swap_out(1_000_000_000, 3, 2), Ok(1_500_000_000));
        assert_eq!(swap_out(1_500_000_000, 2, 3), Ok(1_000_000_000));
        assert_eq!(swap_out(1_000_000_000, 2, 1), Ok(2_000_000_000));
        // The violation row hop.
        assert_eq!(swap_out(1_000_000_000, 1, 2), Ok(500_000_000));
    }

    #[test]
    fn swap_out_floors() {
        assert_eq!(swap_out(7, 1, 2), Ok(3));
        assert_eq!(swap_out(1, 1, 3), Ok(0));
    }

    #[test]
    fn swap_out_uses_u128_intermediate_and_rejects_u64_overflow() {
        // Multiply exceeds u64 but the quotient fits: must succeed.
        assert_eq!(swap_out(u64::MAX, 2, 2), Ok(u64::MAX));
        // Quotient exceeds u64: overflow error.
        assert_eq!(
            swap_out(u64::MAX, 2, 1),
            Err(ProgramError::ArithmeticOverflow)
        );
    }
}
