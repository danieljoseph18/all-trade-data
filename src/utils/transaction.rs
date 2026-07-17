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

/// Resolve the memecoin mint from a pump_swap instruction, skipping non-canonical
/// "scam" pools where `base_mint == WSOL`. Those pools invert the base/quote
/// semantics, so every downstream reserve/fee/market-cap computation would be
/// garbage.
pub fn resolve_pump_swap_memecoin(
    instr: &CompiledInstruction,
    full_accounts: &[Vec<u8>],
) -> Option<String> {
    let mint = resolve_mint_from_instr(instr, full_accounts, PUMP_SWAP_MINT_IX_POS)?;
    if mint.as_str() == WSOL_MINT {
        return None;
    }
    Some(mint)
}

/// Classify the quote mint of a pump_swap instruction. Returns `None` for
/// unsupported quote mints.
pub fn pump_swap_quote_currency(
    instr: &CompiledInstruction,
    full_accounts: &[Vec<u8>],
) -> Option<QuoteCurrency> {
    let mint = resolve_mint_from_instr(instr, full_accounts, PUMP_SWAP_QUOTE_MINT_IX_POS)?;
    quote_currency_of(&mint)
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
        }
        let data = ix.data.as_slice();
        if data.len() >= 16 && &data[8..16] == event_disc {
            return Some(&data[8..]);
        }
    }
    None
}

/// Returns the user (signer) pubkey as a bs58-encoded string.
pub fn get_user(account_keys: &[Vec<u8>]) -> String {
    if account_keys.is_empty() {
        return "unknown".to_string();
    }
    bs58::encode(&account_keys[0]).into_string()
}

// All offsets below are relative to the event discriminator start (data[0..8]),
// matching the layout of PumpSwap BuyEvent / SellEvent.

/// PumpSwap pool reserves: pool_base at 48, pool_quote at 56.
pub fn extract_pool_reserves_from_data(bytes: &[u8]) -> (Option<u64>, Option<u64>) {
    if bytes.len() < 64 {
        return (None, None);
    }
    let base = u64::from_le_bytes(bytes[48..56].try_into().unwrap());
    let quote = u64::from_le_bytes(bytes[56..64].try_into().unwrap());
    (Some(base), Some(quote))
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
        PUMP_SWAP_BUY_EVENT_DISC, PUMP_SWAP_PROGRAM_ID,
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
            find_event_data(&meta, 2, 1, &event_disc).map(|data| &data[..8]),
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

        assert!(find_event_data(&meta, 1, 1, &event_disc).is_none());
    }
}
