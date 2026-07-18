use solana_sdk::{compute_budget, pubkey::Pubkey};
use yellowstone_grpc_proto::prelude::{CompiledInstruction, Message, TransactionStatusMeta};

use super::{
    ASTRALANE_TIP_ADDRESSES, BLOCKRAZOR_TIP_ADDRESSES, BLOCKROUTE_TIP_ADDRESSES,
    FALCON_TIP_ADDRESSES, HELIUS_TIP_ADDRESSES, JITO_TIP_ADDRESSES, MOONLAND_TIP_ADDRESSES,
    NEXTBLOCK_TIP_ADDRESSES, NODE_ONE_TIP_ADDRESSES, PUMP_SWAP_MINT_IX_POS, PUMP_SWAP_POOL_IX_POS,
    PUMP_SWAP_QUOTE_MINT_IX_POS, SOYAS_TIP_ADDRESSES, STELLIUM_TIP_ADDRESSES,
    TEMPORAL_TIP_ADDRESSES, USDC_BASE_UNIT, USDC_MINT, WSOL_MINT, ZEROSLOT_TIP_ADDRESSES,
};

/// Supported quote currency for Pump AMM pools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteCurrency {
    Sol,
    Usdc,
}

impl QuoteCurrency {
    #[inline]
    pub fn is_usdc(self) -> bool {
        matches!(self, QuoteCurrency::Usdc)
    }

    #[inline]
    pub fn base_unit(self) -> f64 {
        match self {
            QuoteCurrency::Sol => super::SOL_BASE_UNIT,
            QuoteCurrency::Usdc => USDC_BASE_UNIT,
        }
    }

    #[inline]
    pub fn label(self) -> &'static str {
        match self {
            QuoteCurrency::Sol => "SOL",
            QuoteCurrency::Usdc => "USDC",
        }
    }
}

/// Classify a quote mint into the supported Pump AMM set.
pub fn quote_currency_of(quote_mint: &str) -> Option<QuoteCurrency> {
    if quote_mint == WSOL_MINT {
        Some(QuoteCurrency::Sol)
    } else if quote_mint == USDC_MINT {
        Some(QuoteCurrency::Usdc)
    } else {
        None
    }
}

/// Resolve the mint from an instruction's account list at a well-known IDL position.
///
/// Direct index lookup — works for failed transactions too (where pre/post token
/// balance scans return nothing because balances don't move).
pub fn resolve_mint_from_instr(
    instr: &CompiledInstruction,
    full_accounts: &[Vec<u8>],
    mint_ix_pos: usize,
) -> Option<String> {
    let acct_idx = *instr.accounts.get(mint_ix_pos)? as usize;
    let bytes = full_accounts.get(acct_idx)?;
    let arr: [u8; 32] = bytes.as_slice().try_into().ok()?;
    Some(Pubkey::new_from_array(arr).to_string())
}

/// Resolve the base mint from any PumpSwap trade instruction.
pub fn resolve_pump_swap_memecoin(
    instr: &CompiledInstruction,
    full_accounts: &[Vec<u8>],
) -> Option<String> {
    resolve_mint_from_instr(instr, full_accounts, PUMP_SWAP_MINT_IX_POS)
}

/// Resolve the quote mint from any PumpSwap trade instruction.
pub fn resolve_pump_swap_quote_mint(
    instr: &CompiledInstruction,
    full_accounts: &[Vec<u8>],
) -> Option<String> {
    resolve_mint_from_instr(instr, full_accounts, PUMP_SWAP_QUOTE_MINT_IX_POS)
}

/// Resolve the pool from any PumpSwap trade instruction.
pub fn resolve_pump_swap_pool(
    instr: &CompiledInstruction,
    full_accounts: &[Vec<u8>],
) -> Option<String> {
    resolve_mint_from_instr(instr, full_accounts, PUMP_SWAP_POOL_IX_POS)
}

/// Resolve an arbitrary instruction account as a base58 pubkey.
pub fn resolve_instruction_account(
    instr: &CompiledInstruction,
    full_accounts: &[Vec<u8>],
    position: usize,
) -> Option<String> {
    resolve_mint_from_instr(instr, full_accounts, position)
}

/// Verify that a PumpSwap trade targets the canonical index-0 pool created by
/// the Pump migration program.
///
/// A zero creator-fee field in the trade event is not a safe canonical-pool
/// signal: legitimate cashback and newer fee configurations can emit zero.
/// Canonical pools have an exact, deterministic PDA instead:
///
/// `pool = PDA("pool", 0u16, pool_authority, base_mint, quote_mint)`
/// `pool_authority = PDA("pool-authority", base_mint)` (Pump program)
pub fn is_canonical_pump_swap_pool(
    instr: &CompiledInstruction,
    full_accounts: &[Vec<u8>],
    pump_program_id: &Pubkey,
    pump_swap_program_id: &Pubkey,
) -> bool {
    fn account_pubkey(
        instr: &CompiledInstruction,
        full_accounts: &[Vec<u8>],
        instruction_position: usize,
    ) -> Option<Pubkey> {
        let account_index = *instr.accounts.get(instruction_position)? as usize;
        let bytes: [u8; 32] = full_accounts
            .get(account_index)?
            .as_slice()
            .try_into()
            .ok()?;
        Some(Pubkey::new_from_array(bytes))
    }

    let Some(pool) = account_pubkey(instr, full_accounts, PUMP_SWAP_POOL_IX_POS) else {
        return false;
    };
    let Some(base_mint) = account_pubkey(instr, full_accounts, PUMP_SWAP_MINT_IX_POS) else {
        return false;
    };
    let Some(quote_mint) = account_pubkey(instr, full_accounts, PUMP_SWAP_QUOTE_MINT_IX_POS) else {
        return false;
    };
    let (pool_authority, _) =
        Pubkey::find_program_address(&[b"pool-authority", base_mint.as_ref()], pump_program_id);
    let pool_index = 0u16.to_le_bytes();
    let (expected_pool, _) = Pubkey::find_program_address(
        &[
            b"pool",
            &pool_index,
            pool_authority.as_ref(),
            base_mint.as_ref(),
            quote_mint.as_ref(),
        ],
        pump_swap_program_id,
    );

    pool == expected_pool
}

/// Locate an Anchor event payload within the inner instructions of a transaction.
///
/// Anchor's `emit_cpi!` writes the instruction data as:
///   `[8-byte anchor event CPI disc][8-byte event disc][borsh event payload]`
///
/// The returned slice starts at the **event disc** (offset 8 in the raw ix data),
/// so field offsets used by the `extract_*` helpers remain stable.
///
/// `parent_outer_idx`: for a top-level trade, its own top-level index; for a CPI
/// trade, the parent top-level index.
/// `start_inner_pos`: position to begin scanning — `0` for top-level trades,
/// `inner_pos + 1` for CPI trades (skip past the trade's own ix).
///
/// The scan stops when it leaves the trade's CPI subtree (i.e. encounters an ix
/// at a `stack_height` at or above the trade's). Without the stack bound, a
/// trade whose own emit is missing or reordered could silently match the next
/// sibling trade's emit and misattribute every field.
pub fn find_event_data<'a>(
    meta: &'a TransactionStatusMeta,
    parent_outer_idx: usize,
    start_inner_pos: usize,
    pump_swap_program_index: u32,
    event_disc: &[u8; 8],
) -> Option<&'a [u8]> {
    let block = meta
        .inner_instructions
        .iter()
        .find(|ii| ii.index as usize == parent_outer_idx)?;

    // Top-level trades run at stack 1; a CPI trade's stack is read from its own
    // ix (which sits at `start_inner_pos - 1`, since callers pass `inner_pos + 1`).
    let trade_stack: u32 = if start_inner_pos == 0 {
        1
    } else {
        block
            .instructions
            .get(start_inner_pos - 1)
            .and_then(|ix| ix.stack_height)
            .unwrap_or(1)
    };

    for ix in block.instructions.iter().skip(start_inner_pos) {
        // Leaving the trade's subtree — next ix belongs to a sibling, stop.
        if let Some(h) = ix.stack_height {
            if h <= trade_stack {
                break;
            }
            // Anchor's emit_cpi! is a direct child of the trade invocation.
            // Ignoring depth can attribute a nested trade's event to its
            // ancestor when a custom program recursively invokes PumpSwap.
            if h != trade_stack + 1 {
                continue;
            }
        }
        // The event must be PumpSwap's self-CPI, not arbitrary instruction
        // data crafted by another program with the same 8-byte discriminator.
        if ix.program_id_index != pump_swap_program_index {
            continue;
        }
        let data = ix.data.as_slice();
        if data.len() >= 16 && &data[8..16] == event_disc {
            return Some(&data[8..]);
        }
    }
    None
}

// All offsets below are relative to the event discriminator start (data[0..8]),
// matching the layout of PumpSwap BuyEvent / SellEvent.

/// PumpSwap pool reserves: pool_base at 48, pool_quote at 56.
///
/// The quote value is the RAW quote-vault balance. Since the 2026-07-15 pool
/// upgrade that is no longer the pricing basis — pair it with
/// `extract_pump_swap_virtual_quote_reserves` and `effective_quote_reserves`.
pub fn extract_pool_reserves_from_data(bytes: &[u8]) -> (Option<u64>, Option<u64>) {
    if bytes.len() < 64 {
        return (None, None);
    }
    let base = u64::from_le_bytes(bytes[48..56].try_into().unwrap());
    let quote = u64::from_le_bytes(bytes[56..64].try_into().unwrap());
    (Some(base), Some(quote))
}

// === PumpSwap virtual quote reserves (appended 2026-07-15) ===
//
// Frozen against pump-fun/pump-public-docs `idl/pump_amm.json` @ 2c22246
// ("feat: pool virtual quotes reserves"), the same upgrade that introduced
// `boost_buy_and_burn`. It *appended* `virtual_quote_reserves` (i128),
// `can_boost` (bool) and `base_supply` (u64) to BuyEvent / SellEvent; no
// pre-existing field moved, so every offset above this line is unchanged.
//
// BuyEvent and SellEvent share a fixed prefix only through `coin_creator_fee`
// (offset 352), then diverge:
//
//   BuyEvent   360 track_volume(bool) 361 total_unclaimed_tokens
//              369 total_claimed_tokens 377 current_sol_volume
//              385 last_update_timestamp 393 min_base_amount_out
//              401 ix_name (VARIABLE-LENGTH Borsh string)
//              then cashback_fee_basis_points, cashback,
//                   buyback_fee_basis_points, buyback_fee, then the field.
//
//   SellEvent  360 cashback_fee_basis_points 368 cashback
//              376 buyback_fee_basis_points 384 buyback_fee
//              392 virtual_quote_reserves
//
// So SellEvent reads at a fixed offset while BuyEvent needs a walker past
// `ix_name`; the two cannot share one offset constant.

/// SellEvent `virtual_quote_reserves` (i128) — fixed offset.
pub const PUMP_SWAP_SELL_VQR_OFFSET: usize = 392;
/// BuyEvent fixed-prefix length; the variable-length `ix_name` starts here.
pub const PUMP_SWAP_BUY_IX_NAME_OFFSET: usize = 401;
/// BuyEvent scalars between `ix_name` and `virtual_quote_reserves`:
/// cashback_fee_basis_points, cashback, buyback_fee_basis_points, buyback_fee.
const PUMP_SWAP_BUY_POST_IX_NAME_SCALARS: usize = 8 * 4;

/// PumpSwap `virtual_quote_reserves` for a buy or sell event.
///
/// Signed (`i128`): the value is an *adjustment* to the quote side, so it is
/// not safe to decode as a `u64`. Returns `None` for a pre-upgrade (or
/// truncated) payload, which callers treat as "no virtual reserves".
///
/// `is_buy` selects the layout; passing the wrong one reads unrelated bytes,
/// so it must match the discriminator the payload was located by.
pub fn extract_pump_swap_virtual_quote_reserves(bytes: &[u8], is_buy: bool) -> Option<i128> {
    let p = if is_buy {
        let mut p = PUMP_SWAP_BUY_IX_NAME_OFFSET;
        // ix_name (Borsh string = u32 length + bytes)
        if p.checked_add(4)? > bytes.len() {
            return None;
        }
        let ix_name_len = u32::from_le_bytes(bytes[p..p + 4].try_into().ok()?) as usize;
        p = p.checked_add(4)?.checked_add(ix_name_len)?;
        p.checked_add(PUMP_SWAP_BUY_POST_IX_NAME_SCALARS)?
    } else {
        PUMP_SWAP_SELL_VQR_OFFSET
    };
    if p.checked_add(16)? > bytes.len() {
        return None;
    }
    Some(i128::from_le_bytes(bytes[p..p + 16].try_into().ok()?))
}

/// PumpSwap effective quote reserves — what buys and sells are actually priced
/// against since the 2026-07-15 pool upgrade:
///
/// ```text
/// effective_quote_reserves = pool_quote_token_account.amount + Pool::virtual_quote_reserves
/// ```
///
/// `virtual_quote_reserves` is signed and may be absent (pre-upgrade payload,
/// or a non-launchpad pool that never carries one), in which case effective
/// reserves are just the raw vault balance. A negative adjustment that would
/// take the pool below zero is clamped — a non-positive quote side has no
/// meaningful price, and `get_market_cap_in_quote` already returns 0 there.
///
/// The base side is unchanged: base reserves stay the raw vault balance.
pub fn effective_quote_reserves(
    raw_quote_reserves: u64,
    virtual_quote_reserves: Option<i128>,
) -> u64 {
    match virtual_quote_reserves {
        Some(virtual_reserves) => (raw_quote_reserves as i128)
            .saturating_add(virtual_reserves)
            .clamp(0, u64::MAX as i128) as u64,
        None => raw_quote_reserves,
    }
}

/// PumpSwap base token amount: base_amount_out@16 (BuyEvent) or base_amount_in@16 (SellEvent).
pub fn extract_transaction_amounts(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 24 {
        return None;
    }
    Some(u64::from_le_bytes(bytes[16..24].try_into().unwrap()))
}

/// Parse the requested `(token_amount, quote_amount)` from a PumpSwap trade
/// instruction's args. Used for **failed** transactions, which emit no
/// BuyEvent/SellEvent self-CPI — the submitted request is the only record of
/// intent, since nothing executed.
///
/// Anchor arg layout, two `u64`s after the 8-byte discriminator:
/// - `buy`:          base_amount_out (token) @8,  max_quote_amount_in (quote) @16
/// - `sell`:         base_amount_in  (token) @8,  min_quote_amount_out (quote) @16
/// - `buy_exact_in`: quote_amount_in (quote) @8,  min_base_amount_out  (token) @16
///
/// `is_exact_in` flips the arg order for the `buy_exact_in` variant. Returns
/// `(0, 0)` when the instruction data is truncated.
pub fn extract_pump_swap_requested_amounts(data: &[u8], is_exact_in: bool) -> (u64, u64) {
    if data.len() < 24 {
        return (0, 0);
    }
    let arg0 = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let arg1 = u64::from_le_bytes(data[16..24].try_into().unwrap());
    if is_exact_in {
        // quote first, base second
        (arg1, arg0)
    } else {
        // base first, quote second
        (arg0, arg1)
    }
}

/// PumpSwap quote amounts: buy_volume @112 = user_quote_amount_in,
/// sell_volume @64 = quote_amount_out. SOL pools return lamports; USDC pools
/// return USDC microunits.
pub fn extract_quote_volume(bytes: &[u8]) -> (Option<u64>, Option<u64>) {
    if bytes.len() < 120 {
        return (None, None);
    }
    let sell = u64::from_le_bytes(bytes[64..72].try_into().unwrap());
    let buy = u64::from_le_bytes(bytes[112..120].try_into().unwrap());
    (Some(buy), Some(sell))
}

/// Decode the stable prefix of BoostBuyAndBurnEvent.
/// Returns `(base_burned, quote_used, base_reserves_after, effective_quote_after)`.
pub fn extract_boost_buy_event(bytes: &[u8]) -> Option<(u64, u64, u64, u64)> {
    if bytes.len() < 200 {
        return None;
    }
    let quote_used = u64::from_le_bytes(bytes[152..160].try_into().ok()?);
    let base_burned = u64::from_le_bytes(bytes[160..168].try_into().ok()?);
    let virtual_quote = i128::from_le_bytes(bytes[168..184].try_into().ok()?);
    let real_quote = u64::from_le_bytes(bytes[184..192].try_into().ok()?);
    let base_after = u64::from_le_bytes(bytes[192..200].try_into().ok()?);
    let effective_quote = effective_quote_reserves(real_quote, Some(virtual_quote));
    Some((base_burned, quote_used, base_after, effective_quote))
}

/// Compute market cap from post-trade reserves without applying a trade delta.
pub fn get_market_cap_from_reserves(
    base_reserves: u64,
    quote_reserves: u64,
    quote_currency: QuoteCurrency,
) -> u64 {
    get_market_cap_in_quote(base_reserves, quote_reserves, 0, 0, false, quote_currency)
}

/// Collects all instructions (direct + CPI) for a specific program ID from a transaction.
///
/// Returns `(instr, parent_outer_idx, start_inner_pos)`:
/// - `parent_outer_idx`: for top-level calls, the instruction's own top-level index;
///   for CPIs, the parent top-level index (i.e. which `inner_instructions` block it lives in).
/// - `start_inner_pos`: `0` for top-level trades (scan the whole inner block to find the
///   event emit); `inner_position + 1` for CPI trades (skip past the trade's own ix so
///   `find_event_data` lands on the emit that follows).
pub fn get_program_instructions(
    msg: &Message,
    meta: &TransactionStatusMeta,
    full_accounts: &[Vec<u8>],
    program_id: &Pubkey,
) -> Vec<(CompiledInstruction, usize, usize)> {
    let program_index = full_accounts.iter().position(|key| {
        <[u8; 32]>::try_from(key.as_slice())
            .map(|arr| Pubkey::new_from_array(arr) == *program_id)
            .unwrap_or(false)
    });

    let mut all_instrs: Vec<(CompiledInstruction, usize, usize)> = Vec::new();

    if let Some(program_index) = program_index {
        // top-level
        for (i, instr) in msg
            .instructions
            .iter()
            .enumerate()
            .filter(|(_, ix)| ix.program_id_index as usize == program_index)
        {
            all_instrs.push((instr.clone(), i, 0));
        }
        // inner (CPI)
        for inner in &meta.inner_instructions {
            let parent_idx = inner.index as usize;
            for (j, ix) in inner
                .instructions
                .iter()
                .enumerate()
                .filter(|(_, i)| i.program_id_index as usize == program_index)
            {
                let compiled_instr = CompiledInstruction {
                    program_id_index: ix.program_id_index,
                    accounts: ix.accounts.clone(),
                    data: ix.data.clone(),
                };
                all_instrs.push((compiled_instr, parent_idx, j + 1));
            }
        }
    }

    all_instrs
}

/// Lookup a known tip-provider by base58 pubkey string. Returns the provider
/// label that should be persisted as `tip_provider`. `None` means the address
/// isn't a known landing service.
///
/// Only third-party landing services that any market participant can pay are
/// included. Our own RPC-tip pubkeys (GadFly / Helius RPC / Triton RPC) and
/// the "Harmonic" sentinel are intentionally excluded — those are signals
/// specific to our own infrastructure and would only show up for our own
/// trades, not the general AMM flow this collector observes.
fn lookup_tip_provider(pubkey: &str) -> Option<&'static str> {
    if TEMPORAL_TIP_ADDRESSES.contains(&pubkey) {
        Some("Temporal")
    } else if NEXTBLOCK_TIP_ADDRESSES.contains(&pubkey) {
        Some("NextBlock")
    } else if JITO_TIP_ADDRESSES.contains(&pubkey) {
        Some("Jito")
    } else if ZEROSLOT_TIP_ADDRESSES.contains(&pubkey) {
        Some("ZeroSlot")
    } else if BLOCKROUTE_TIP_ADDRESSES.contains(&pubkey) {
        Some("BlockRoute")
    } else if NODE_ONE_TIP_ADDRESSES.contains(&pubkey) {
        Some("NodeOne")
    } else if ASTRALANE_TIP_ADDRESSES.contains(&pubkey) {
        Some("Astralane")
    } else if BLOCKRAZOR_TIP_ADDRESSES.contains(&pubkey) {
        Some("BlockRazor")
    } else if HELIUS_TIP_ADDRESSES.contains(&pubkey) {
        Some("HeliusSender")
    } else if STELLIUM_TIP_ADDRESSES.contains(&pubkey) {
        Some("Stellium")
    } else if SOYAS_TIP_ADDRESSES.contains(&pubkey) {
        Some("Soyas")
    } else if MOONLAND_TIP_ADDRESSES.contains(&pubkey) {
        Some("Moonland")
    } else if FALCON_TIP_ADDRESSES.contains(&pubkey) {
        Some("Falcon")
    } else {
        None
    }
}

/// Extract `(priority_fee_microlamports, transfer_tip_lamports, tip_provider)`.
///
/// - **priority_fee**: parsed from a compute-budget `SetComputeUnitPrice` ix
///   (tag byte `3`, u64 little-endian payload). Microlamports per CU.
/// - **transfer_tip**: positive lamport delta on a known tip-provider account
///   (pre→post balance). First matching transfer wins.
/// - **tip_provider**: the label of the matched provider. When no transfer
///   matches but a priority fee was set, the trade was landed via the raw
///   validator path and is labeled `"TPU"`. With neither a transfer nor a
///   priority fee, fall back to `"RPC"` — sent through a regular RPC node with
///   no landing accelerant.
pub fn extract_transaction_fees(
    msg: &Message,
    meta: &TransactionStatusMeta,
    full_accounts: &[Vec<u8>],
) -> (Option<u64>, Option<u64>, Option<String>) {
    let compute_budget_program_id = compute_budget::id();

    let compute_budget_index = full_accounts.iter().position(|key| {
        <[u8; 32]>::try_from(key.as_slice())
            .map(|arr| Pubkey::new_from_array(arr) == compute_budget_program_id)
            .unwrap_or(false)
    });

    let mut priority_fee: Option<u64> = None;
    if let Some(idx) = compute_budget_index {
        for instr in &msg.instructions {
            if instr.program_id_index as usize == idx && instr.data.len() >= 9 && instr.data[0] == 3
            {
                priority_fee = Some(u64::from_le_bytes(instr.data[1..9].try_into().unwrap()));
            }
        }
    }

    let mut tip_provider: Option<String> = None;
    let mut transfer_tip: Option<u64> = None;

    let pre_balances = &meta.pre_balances;
    let post_balances = &meta.post_balances;

    for (idx, account) in full_accounts.iter().enumerate() {
        let Ok(arr) = <[u8; 32]>::try_from(account.as_slice()) else {
            continue;
        };
        let pubkey_str = Pubkey::new_from_array(arr).to_string();
        if let Some(service) = lookup_tip_provider(&pubkey_str) {
            if idx < pre_balances.len() && idx < post_balances.len() {
                let pre = pre_balances[idx];
                let post = post_balances[idx];
                if post > pre {
                    transfer_tip = Some(post - pre);
                    tip_provider = Some(service.to_string());
                    break;
                }
            }
        }
    }

    if tip_provider.is_none() {
        if priority_fee.unwrap_or(0) > 0 {
            // Priority fee paid with no transfer tip — landed directly via the
            // validator TPU, no third-party landing service.
            tip_provider = Some("TPU".to_string());
        } else {
            // No tip, no priority fee — plain RPC submission.
            tip_provider = Some("RPC".to_string());
        }
    }

    (priority_fee, transfer_tip, tip_provider)
}

/// Calculates market cap in the pool quote currency's smallest unit.
pub fn get_market_cap_in_quote(
    pool_base: u64,
    pool_quote: u64,
    token_amount: u64,
    quote_amount: u64,
    is_buy: bool,
    quote_currency: QuoteCurrency,
) -> u64 {
    if pool_base == 0 {
        return 0;
    }
    let mut base_real = pool_base as f64 / 1_000_000.0;
    let quote_base_unit = quote_currency.base_unit();
    let mut quote_real = pool_quote as f64 / quote_base_unit;

    if is_buy {
        quote_real += quote_amount as f64 / quote_base_unit;
        base_real -= token_amount as f64 / 1_000_000.0;
    } else {
        quote_real -= quote_amount as f64 / quote_base_unit;
        base_real += token_amount as f64 / 1_000_000.0;
    }

    if base_real <= 0.0 {
        return 0;
    }

    let price_per_token_in_quote = quote_real / base_real;
    let total_supply = 1_000_000_000u64;

    (price_per_token_in_quote * total_supply as f64 * quote_base_unit).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::{
        AMM_SELL_DISCRIMINATOR, BUY_EXACT_IN_DISCRIMINATOR, PUMP_PROGRAM_ID,
        PUMP_SWAP_BUY_EVENT_DISC, PUMP_SWAP_PROGRAM_ID, PUMP_SWAP_SELL_EVENT_DISC,
    };
    use std::str::FromStr;
    use yellowstone_grpc_proto::prelude::{InnerInstruction, InnerInstructions};

    #[test]
    fn quote_currency_supports_sol_and_usdc_only() {
        assert_eq!(quote_currency_of(WSOL_MINT), Some(QuoteCurrency::Sol));
        assert_eq!(quote_currency_of(USDC_MINT), Some(QuoteCurrency::Usdc));
        assert_eq!(quote_currency_of("11111111111111111111111111111111"), None);
    }

    #[test]
    fn market_cap_uses_quote_currency_base_unit() {
        let sol_mc = get_market_cap_in_quote(
            100_000_000_000_000,
            500_000_000_000,
            0,
            0,
            true,
            QuoteCurrency::Sol,
        );
        let usdc_mc = get_market_cap_in_quote(
            100_000_000_000_000,
            500_000_000,
            0,
            0,
            true,
            QuoteCurrency::Usdc,
        );

        assert_eq!(sol_mc, 5_000_000_000_000);
        assert_eq!(usdc_mc, 5_000_000_000);
    }

    #[test]
    fn recognizes_reproduced_cpi_fills_as_canonical_pools() {
        // On-chain CPI buy and sell from slot 433339880. Both events have
        // coin_creator_fee_basis_points == 0, which the old sentinel rejected.
        let pump_program = Pubkey::from_str(PUMP_PROGRAM_ID).unwrap();
        let pump_swap_program = Pubkey::from_str(PUMP_SWAP_PROGRAM_ID).unwrap();
        let quote = Pubkey::from_str(WSOL_MINT).unwrap();
        let cases = [
            (
                "Fr8astXNXz2dJDfvwCQnmqGprADkH89GSbHT5eRwfwMy",
                "59dCaphi38eZHiuZjJ2n26BuEcktu9ohdH8eAQd8pump",
                BUY_EXACT_IN_DISCRIMINATOR,
            ),
            (
                "GQf9SMW9UVkJTiTibWjLSSQWFAhRvdHi58cyBw5y33NR",
                "6PS6MdRgu3GdfL1ZW25v9xqVFhjBQYLf6yXEndpwY1ko",
                AMM_SELL_DISCRIMINATOR,
            ),
        ];

        for (pool, base, discriminator) in cases {
            let full_accounts = vec![
                Pubkey::from_str(pool).unwrap().to_bytes().to_vec(),
                Pubkey::new_unique().to_bytes().to_vec(),
                Pubkey::new_unique().to_bytes().to_vec(),
                Pubkey::from_str(base).unwrap().to_bytes().to_vec(),
                quote.to_bytes().to_vec(),
            ];
            let instr = CompiledInstruction {
                program_id_index: 5,
                accounts: vec![0, 1, 2, 3, 4],
                data: discriminator.to_vec(),
            };

            assert!(is_canonical_pump_swap_pool(
                &instr,
                &full_accounts,
                &pump_program,
                &pump_swap_program,
            ));

            let mut spoofed_accounts = full_accounts;
            spoofed_accounts[0] = Pubkey::new_unique().to_bytes().to_vec();
            assert!(!is_canonical_pump_swap_pool(
                &instr,
                &spoofed_accounts,
                &pump_program,
                &pump_swap_program,
            ));
        }
    }

    #[test]
    fn finds_event_inside_nested_cpi_trade_subtree() {
        let event_disc = PUMP_SWAP_BUY_EVENT_DISC;
        let mut event_data = vec![0u8; 16];
        event_data[8..16].copy_from_slice(&event_disc);

        let meta = TransactionStatusMeta {
            inner_instructions: vec![InnerInstructions {
                index: 2,
                instructions: vec![
                    InnerInstruction {
                        program_id_index: 7,
                        data: BUY_EXACT_IN_DISCRIMINATOR.to_vec(),
                        stack_height: Some(3),
                        ..Default::default()
                    },
                    InnerInstruction {
                        program_id_index: 8,
                        data: vec![],
                        stack_height: Some(4),
                        ..Default::default()
                    },
                    InnerInstruction {
                        program_id_index: 7,
                        data: event_data,
                        stack_height: Some(4),
                        ..Default::default()
                    },
                ],
            }],
            ..Default::default()
        };

        assert_eq!(
            find_event_data(&meta, 2, 1, 7, &event_disc).map(|data| &data[..8]),
            Some(event_disc.as_slice()),
        );
    }

    #[test]
    fn does_not_borrow_event_from_sibling_cpi_trade() {
        let event_disc = PUMP_SWAP_BUY_EVENT_DISC;
        let mut sibling_event = vec![0u8; 16];
        sibling_event[8..16].copy_from_slice(&event_disc);

        let meta = TransactionStatusMeta {
            inner_instructions: vec![InnerInstructions {
                index: 1,
                instructions: vec![
                    InnerInstruction {
                        program_id_index: 7,
                        data: BUY_EXACT_IN_DISCRIMINATOR.to_vec(),
                        stack_height: Some(2),
                        ..Default::default()
                    },
                    InnerInstruction {
                        program_id_index: 7,
                        data: BUY_EXACT_IN_DISCRIMINATOR.to_vec(),
                        stack_height: Some(2),
                        ..Default::default()
                    },
                    InnerInstruction {
                        program_id_index: 7,
                        data: sibling_event,
                        stack_height: Some(3),
                        ..Default::default()
                    },
                ],
            }],
            ..Default::default()
        };

        assert!(find_event_data(&meta, 1, 1, 7, &event_disc).is_none());
    }

    // === PumpSwap event decoding ===

    /// Fields a synthetic PumpSwap event carries. Everything not named here is
    /// zero-filled; the builders place each field at its IDL offset.
    #[derive(Default, Clone)]
    struct PumpSwapEventFields {
        /// base_amount_out (buy) / base_amount_in (sell) @16
        base_amount: u64,
        /// @48
        pool_base_token_reserves: u64,
        /// @56 — the RAW quote-vault balance, not the effective reserves.
        pool_quote_token_reserves: u64,
        /// quote_amount_in (buy) / quote_amount_out (sell) @64
        quote_amount: u64,
        /// user_quote_amount_in (buy) / user_quote_amount_out (sell) @112
        user_quote_amount: u64,
        /// BuyEvent only — variable-length, which is what forces the walker.
        ix_name: String,
        /// Appended 2026-07-15. Signed.
        virtual_quote_reserves: i128,
        /// Appended 2026-07-15.
        base_supply: u64,
    }

    /// Bytes shared by BuyEvent and SellEvent: the event discriminator through
    /// `coin_creator_fee` @352, where the two layouts diverge.
    fn build_pump_swap_common_prefix(disc: &[u8; 8], f: &PumpSwapEventFields) -> Vec<u8> {
        let mut b = Vec::with_capacity(512);
        b.extend_from_slice(disc);
        b.extend_from_slice(&0i64.to_le_bytes()); // 8   timestamp
        b.extend_from_slice(&f.base_amount.to_le_bytes()); // 16  base_amount_{out,in}
        b.extend_from_slice(&0u64.to_le_bytes()); // 24  {max,min}_quote_amount
        b.extend_from_slice(&0u64.to_le_bytes()); // 32  user_base_token_reserves
        b.extend_from_slice(&0u64.to_le_bytes()); // 40  user_quote_token_reserves
        assert_eq!(b.len(), 48, "pool_base_token_reserves offset");
        b.extend_from_slice(&f.pool_base_token_reserves.to_le_bytes()); // 48
        b.extend_from_slice(&f.pool_quote_token_reserves.to_le_bytes()); // 56
        assert_eq!(b.len(), 64, "quote_amount offset");
        b.extend_from_slice(&f.quote_amount.to_le_bytes()); // 64
        b.extend_from_slice(&0u64.to_le_bytes()); // 72  lp_fee_basis_points
        b.extend_from_slice(&0u64.to_le_bytes()); // 80  lp_fee
        b.extend_from_slice(&0u64.to_le_bytes()); // 88  protocol_fee_basis_points
        b.extend_from_slice(&0u64.to_le_bytes()); // 96  protocol_fee
        b.extend_from_slice(&0u64.to_le_bytes()); // 104 quote_amount_*_lp_fee
        assert_eq!(b.len(), 112, "user_quote_amount offset");
        b.extend_from_slice(&f.user_quote_amount.to_le_bytes()); // 112
        // 120 pool, 152 user, 184 user_base_ta, 216 user_quote_ta,
        // 248 protocol_fee_recipient, 280 protocol_fee_recipient_ta
        b.extend(std::iter::repeat_n(0u8, 32 * 6));
        assert_eq!(b.len(), 312, "coin_creator offset");
        b.extend(std::iter::repeat_n(0u8, 32)); // 312 coin_creator
        b.extend_from_slice(&0u64.to_le_bytes()); // 344 coin_creator_fee_basis_points
        b.extend_from_slice(&0u64.to_le_bytes()); // 352 coin_creator_fee
        assert_eq!(b.len(), 360, "end of shared prefix");
        b
    }

    /// Synthetic BuyEvent matching `idl/pump_amm.json` @ 2c22246.
    /// `with_appended_fields = false` reproduces a pre-2026-07-15 payload.
    fn build_pump_swap_buy_event(f: &PumpSwapEventFields, with_appended_fields: bool) -> Vec<u8> {
        let mut b = build_pump_swap_common_prefix(&PUMP_SWAP_BUY_EVENT_DISC, f);
        b.push(0); // 360 track_volume (bool)
        b.extend_from_slice(&0u64.to_le_bytes()); // 361 total_unclaimed_tokens
        b.extend_from_slice(&0u64.to_le_bytes()); // 369 total_claimed_tokens
        b.extend_from_slice(&0u64.to_le_bytes()); // 377 current_sol_volume
        b.extend_from_slice(&0i64.to_le_bytes()); // 385 last_update_timestamp
        b.extend_from_slice(&0u64.to_le_bytes()); // 393 min_base_amount_out
        assert_eq!(b.len(), PUMP_SWAP_BUY_IX_NAME_OFFSET, "ix_name offset");
        b.extend_from_slice(&(f.ix_name.len() as u32).to_le_bytes());
        b.extend_from_slice(f.ix_name.as_bytes());
        b.extend_from_slice(&0u64.to_le_bytes()); // cashback_fee_basis_points
        b.extend_from_slice(&0u64.to_le_bytes()); // cashback
        b.extend_from_slice(&0u64.to_le_bytes()); // buyback_fee_basis_points
        b.extend_from_slice(&0u64.to_le_bytes()); // buyback_fee
        if with_appended_fields {
            b.extend_from_slice(&f.virtual_quote_reserves.to_le_bytes());
            b.push(0); // can_boost
            b.extend_from_slice(&f.base_supply.to_le_bytes());
        }
        b
    }

    /// Synthetic SellEvent matching `idl/pump_amm.json` @ 2c22246.
    /// Unlike BuyEvent this layout is fully fixed-size.
    fn build_pump_swap_sell_event(f: &PumpSwapEventFields, with_appended_fields: bool) -> Vec<u8> {
        let mut b = build_pump_swap_common_prefix(&PUMP_SWAP_SELL_EVENT_DISC, f);
        b.extend_from_slice(&0u64.to_le_bytes()); // 360 cashback_fee_basis_points
        b.extend_from_slice(&0u64.to_le_bytes()); // 368 cashback
        b.extend_from_slice(&0u64.to_le_bytes()); // 376 buyback_fee_basis_points
        b.extend_from_slice(&0u64.to_le_bytes()); // 384 buyback_fee
        assert_eq!(
            b.len(),
            PUMP_SWAP_SELL_VQR_OFFSET,
            "virtual_quote_reserves offset"
        );
        if with_appended_fields {
            b.extend_from_slice(&f.virtual_quote_reserves.to_le_bytes()); // 392
            b.push(0); // 408 can_boost
            b.extend_from_slice(&f.base_supply.to_le_bytes()); // 409
        }
        b
    }

    /// Realistic freshly-migrated pump.fun pool: ~85 SOL of real quote against
    /// ~206.9M tokens, plus the kind of virtual top-up phase 2 introduces.
    fn migrated_pool_fields() -> PumpSwapEventFields {
        PumpSwapEventFields {
            base_amount: 1_000_000_000,
            pool_base_token_reserves: 206_900_000_000_000,
            pool_quote_token_reserves: 85_000_000_000, // 85 SOL raw
            quote_amount: 500_000_000,
            user_quote_amount: 505_000_000,
            ix_name: "buy".to_string(),
            virtual_quote_reserves: 30_000_000_000, // 30 SOL virtual
            base_supply: 1_000_000_000_000_000,
        }
    }

    /// Offsets are frozen against the IDL; the builders assert them
    /// structurally, this pins the constants the decoder itself uses.
    #[test]
    fn pump_swap_vqr_offsets_match_idl() {
        assert_eq!(PUMP_SWAP_SELL_VQR_OFFSET, 392);
        assert_eq!(PUMP_SWAP_BUY_IX_NAME_OFFSET, 401);
        assert_eq!(PUMP_SWAP_BUY_POST_IX_NAME_SCALARS, 32);
    }

    #[test]
    fn pump_swap_sell_event_reads_virtual_quote_reserves() {
        let bytes = build_pump_swap_sell_event(&migrated_pool_fields(), true);
        assert_eq!(
            extract_pump_swap_virtual_quote_reserves(&bytes, false),
            Some(30_000_000_000)
        );
    }

    /// The BuyEvent walker must land on the same field regardless of how long
    /// `ix_name` is — the reason a fixed offset cannot be used there.
    #[test]
    fn pump_swap_buy_event_walks_variable_length_ix_name() {
        for ix_name in ["", "buy", "buy_exact_quote_in", &"x".repeat(255)] {
            let f = PumpSwapEventFields {
                ix_name: ix_name.to_string(),
                ..migrated_pool_fields()
            };
            let bytes = build_pump_swap_buy_event(&f, true);
            assert_eq!(
                extract_pump_swap_virtual_quote_reserves(&bytes, true),
                Some(30_000_000_000),
                "ix_name len {}",
                ix_name.len()
            );
        }
    }

    /// The field is `i128`, so a negative adjustment must survive decoding
    /// rather than wrapping into a huge positive number.
    #[test]
    fn pump_swap_virtual_quote_reserves_decodes_negative() {
        let f = PumpSwapEventFields {
            virtual_quote_reserves: -5_000_000_000,
            ..migrated_pool_fields()
        };
        assert_eq!(
            extract_pump_swap_virtual_quote_reserves(&build_pump_swap_buy_event(&f, true), true),
            Some(-5_000_000_000)
        );
        assert_eq!(
            extract_pump_swap_virtual_quote_reserves(&build_pump_swap_sell_event(&f, true), false),
            Some(-5_000_000_000)
        );
    }

    /// A pre-2026-07-15 payload has no appended fields — decode must degrade to
    /// None (treated as "no virtual reserves") rather than reading garbage.
    #[test]
    fn pump_swap_virtual_quote_reserves_none_on_pre_upgrade_payload() {
        let f = migrated_pool_fields();
        assert_eq!(
            extract_pump_swap_virtual_quote_reserves(&build_pump_swap_buy_event(&f, false), true),
            None
        );
        assert_eq!(
            extract_pump_swap_virtual_quote_reserves(&build_pump_swap_sell_event(&f, false), false),
            None
        );
    }

    /// The layouts diverge after offset 352, so the buy/sell flag must match
    /// the discriminator the payload was found by.
    #[test]
    fn pump_swap_virtual_quote_reserves_wrong_layout_does_not_alias() {
        let f = migrated_pool_fields();
        assert_ne!(
            extract_pump_swap_virtual_quote_reserves(&build_pump_swap_sell_event(&f, true), true),
            Some(30_000_000_000)
        );
        assert_ne!(
            extract_pump_swap_virtual_quote_reserves(&build_pump_swap_buy_event(&f, true), false),
            Some(30_000_000_000)
        );
    }

    /// A corrupt / hostile ix_name length must not panic or index out of bounds.
    #[test]
    fn pump_swap_virtual_quote_reserves_survives_absurd_ix_name_len() {
        let mut bytes = build_pump_swap_buy_event(&migrated_pool_fields(), true);
        let p = PUMP_SWAP_BUY_IX_NAME_OFFSET;
        bytes[p..p + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(extract_pump_swap_virtual_quote_reserves(&bytes, true), None);
    }

    /// Every truncation of a valid payload must return None, never panic.
    #[test]
    fn pump_swap_decoders_never_panic_on_truncated_payloads() {
        let f = migrated_pool_fields();
        for full in [
            build_pump_swap_buy_event(&f, true),
            build_pump_swap_sell_event(&f, true),
        ] {
            for len in 0..full.len() {
                let bytes = &full[..len];
                let _ = extract_pump_swap_virtual_quote_reserves(bytes, true);
                let _ = extract_pump_swap_virtual_quote_reserves(bytes, false);
                let _ = extract_pool_reserves_from_data(bytes);
                let _ = extract_quote_volume(bytes);
                let _ = extract_transaction_amounts(bytes);
                let _ = extract_boost_buy_event(bytes);
            }
        }
    }

    /// Regression guard for the 2026-07-15 upgrade: the new BuyEvent/SellEvent
    /// fields were *appended*, so every pre-existing decoder must still read
    /// its field correctly out of a new-format payload. If pump ever inserts a
    /// field mid-struct instead, this is what catches it.
    #[test]
    fn pump_swap_appended_fields_do_not_shift_existing_offsets() {
        let f = migrated_pool_fields();
        for (bytes, is_buy) in [
            (build_pump_swap_buy_event(&f, true), true),
            (build_pump_swap_sell_event(&f, true), false),
        ] {
            assert_eq!(
                extract_pool_reserves_from_data(&bytes),
                (Some(206_900_000_000_000), Some(85_000_000_000)),
                "pool reserves (is_buy={is_buy})"
            );
            assert_eq!(
                extract_transaction_amounts(&bytes),
                Some(1_000_000_000),
                "base amount (is_buy={is_buy})"
            );
            let (buy_vol, sell_vol) = extract_quote_volume(&bytes);
            assert_eq!(buy_vol, Some(505_000_000), "buy volume (is_buy={is_buy})");
            assert_eq!(sell_vol, Some(500_000_000), "sell volume (is_buy={is_buy})");
        }
    }

    // === effective quote reserves ===

    const RAW_QUOTE: u64 = 85_000_000_000;

    #[test]
    fn effective_quote_reserves_absent_field_is_raw_balance() {
        assert_eq!(effective_quote_reserves(RAW_QUOTE, None), RAW_QUOTE);
    }

    /// Phase 1 shipped the field as 0 on every pool, so it must be a no-op.
    #[test]
    fn effective_quote_reserves_zero_is_no_op() {
        assert_eq!(effective_quote_reserves(RAW_QUOTE, Some(0)), RAW_QUOTE);
    }

    #[test]
    fn effective_quote_reserves_applies_signed_adjustment() {
        assert_eq!(
            effective_quote_reserves(RAW_QUOTE, Some(30_000_000_000)),
            115_000_000_000
        );
        assert_eq!(
            effective_quote_reserves(RAW_QUOTE, Some(-5_000_000_000)),
            80_000_000_000
        );
    }

    /// A negative adjustment larger than the vault balance would make the quote
    /// side non-positive; clamp rather than wrap through the u64 cast.
    #[test]
    fn effective_quote_reserves_clamps_and_saturates() {
        assert_eq!(effective_quote_reserves(RAW_QUOTE, Some(i128::MIN)), 0);
        assert_eq!(
            effective_quote_reserves(RAW_QUOTE, Some(-(RAW_QUOTE as i128) - 1)),
            0
        );
        assert_eq!(
            effective_quote_reserves(u64::MAX, Some(i128::MAX)),
            u64::MAX
        );
    }

    // === boost_buy_and_burn ===

    /// BoostBuyAndBurnEvent shipped with the same upgrade and carries
    /// `virtual_quote_reserves` (i128) *mid-struct* at 168, ahead of
    /// `real_quote_reserves_after` — so it is decoded by fixed offset, unlike
    /// the appended buy/sell field. Reported quote must already be effective.
    #[test]
    fn boost_buy_event_reports_effective_quote_reserves() {
        let build = |virtual_quote: i128| {
            let mut b = vec![0u8; 200];
            b[152..160].copy_from_slice(&7_000_000u64.to_le_bytes()); // quote_used
            b[160..168].copy_from_slice(&3_000_000u64.to_le_bytes()); // base_burned
            b[168..184].copy_from_slice(&virtual_quote.to_le_bytes()); // i128
            b[184..192].copy_from_slice(&85_000_000_000u64.to_le_bytes()); // real quote
            b[192..200].copy_from_slice(&206_900_000_000_000u64.to_le_bytes()); // base after
            b
        };

        assert_eq!(
            extract_boost_buy_event(&build(0)),
            Some((3_000_000, 7_000_000, 206_900_000_000_000, 85_000_000_000))
        );
        assert_eq!(
            extract_boost_buy_event(&build(30_000_000_000)),
            Some((3_000_000, 7_000_000, 206_900_000_000_000, 115_000_000_000))
        );
        // Negative adjustment applies, and over-subtraction clamps to zero.
        assert_eq!(
            extract_boost_buy_event(&build(-5_000_000_000)).map(|t| t.3),
            Some(80_000_000_000)
        );
        assert_eq!(
            extract_boost_buy_event(&build(i128::MIN)).map(|t| t.3),
            Some(0)
        );
        // Truncated payload must not panic.
        assert_eq!(extract_boost_buy_event(&build(0)[..199]), None);
    }
}
