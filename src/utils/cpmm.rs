//! Pure parsers and math for Raydium CPMM swaps (stonk.fun post-migration).
//!
//! No I/O. Layouts follow `raydium_cp_swap` IDL v0.2.0 and are verified
//! against a live mainnet transaction in the tests below. Mirrors the CPMM
//! half of strategy-data's `utils/launchlab.rs` so both collectors decode a
//! swap identically.
//!
//! CPMM emits `SwapEvent` through Anchor `emit!` (a `Program data:` log
//! line), not `emit_cpi`, so there is no self-CPI inner instruction to scope
//! by. Events are matched back to instructions by `pool_id`, in execution
//! order.

use base64::Engine;
use solana_program::pubkey::Pubkey;
use yellowstone_grpc_proto::prelude::{CompiledInstruction, TokenBalance, TransactionStatusMeta};

use super::{CPMM_POOL_SEED, CPMM_SWAP_EVENT_DISC};

/// Fixed byte length of the CPMM `SwapEvent` body (after the disc).
pub const CPMM_SWAP_EVENT_BODY_LEN: usize = 162;

fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

fn pubkey_at(b: &[u8], off: usize) -> Option<String> {
    Some(bs58::encode(b.get(off..off + 32)?).into_string())
}

/// Raydium CPMM `SwapEvent`.
///
/// `*_vault_before` are the vault balances net of accrued fees; the
/// post-swap reserves are `input_vault_before + input_amount` and
/// `output_vault_before - output_amount`. Amounts exclude Token-2022 transfer
/// fees, which are reported separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpmmSwapEvent {
    /// CPMM `PoolState`, base58.
    pub pool_id: String,
    pub input_vault_before: u64,
    pub output_vault_before: u64,
    pub input_amount: u64,
    pub output_amount: u64,
    pub input_transfer_fee: u64,
    pub output_transfer_fee: u64,
    /// True for `swap_base_input` (exact-in), false for `swap_base_output`.
    pub base_input: bool,
    pub input_mint: String,
    pub output_mint: String,
    pub trade_fee: u64,
    pub creator_fee: u64,
    pub creator_fee_on_input: bool,
}

impl CpmmSwapEvent {
    /// Parse from a slice that starts with the 8-byte event discriminator
    /// (what `find_event_data` returns, or a decoded `Program data:` line).
    /// Returns `None` unless the body is exactly [`CPMM_SWAP_EVENT_BODY_LEN`]
    /// bytes: the Raydium CLMM `SwapEvent` shares the discriminator with a
    /// 213-byte body.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let (disc, b) = data.split_at_checked(8)?;
        if disc != CPMM_SWAP_EVENT_DISC || b.len() != CPMM_SWAP_EVENT_BODY_LEN {
            return None;
        }
        Some(Self {
            pool_id: pubkey_at(b, 0)?,
            input_vault_before: u64_at(b, 32)?,
            output_vault_before: u64_at(b, 40)?,
            input_amount: u64_at(b, 48)?,
            output_amount: u64_at(b, 56)?,
            input_transfer_fee: u64_at(b, 64)?,
            output_transfer_fee: u64_at(b, 72)?,
            base_input: *b.get(80)? != 0,
            input_mint: pubkey_at(b, 81)?,
            output_mint: pubkey_at(b, 113)?,
            trade_fee: u64_at(b, 145)?,
            creator_fee: u64_at(b, 153)?,
            creator_fee_on_input: *b.get(161)? != 0,
        })
    }
}

/// A CPMM swap re-expressed from the tracked base mint's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpmmSwapSide {
    /// True when the user received base (quote in, base out).
    pub is_buy: bool,
    /// Base amount as seen by the pool (net of the Token-2022 transfer fee).
    pub base_amount: u64,
    pub quote_amount: u64,
    pub base_reserve_after: u64,
    pub quote_reserve_after: u64,
    /// Token-2022 transfer fee charged on the base leg.
    pub base_transfer_fee: u64,
}

/// Resolve which side of a CPMM swap the tracked base mint was on.
///
/// CPMM `token_0` / `token_1` are pubkey-sorted, so the side is resolved by
/// matching `tracked_base_mint` against the event's `input_mint` /
/// `output_mint` — never by position or vault size. Returns `None` when the
/// event does not involve the tracked mint.
pub fn cpmm_resolve_side(ev: &CpmmSwapEvent, tracked_base_mint: &str) -> Option<CpmmSwapSide> {
    if ev.input_mint == tracked_base_mint {
        Some(CpmmSwapSide {
            is_buy: false,
            base_amount: ev.input_amount,
            quote_amount: ev.output_amount,
            base_reserve_after: ev.input_vault_before.saturating_add(ev.input_amount),
            quote_reserve_after: ev.output_vault_before.saturating_sub(ev.output_amount),
            base_transfer_fee: ev.input_transfer_fee,
        })
    } else if ev.output_mint == tracked_base_mint {
        Some(CpmmSwapSide {
            is_buy: true,
            base_amount: ev.output_amount,
            quote_amount: ev.input_amount,
            base_reserve_after: ev.output_vault_before.saturating_sub(ev.output_amount),
            quote_reserve_after: ev.input_vault_before.saturating_add(ev.input_amount),
            base_transfer_fee: ev.output_transfer_fee,
        })
    } else {
        None
    }
}

/// CPMM market cap in quote atomic units: `quote_reserve * supply / base_reserve`.
/// `supply` is the create-time supply (10^15 for every stonk.fun launch so
/// far), not the circulating amount. u128 intermediates, saturating; 0 when
/// `base_reserve == 0`.
pub fn cpmm_market_cap(quote_reserve: u64, base_reserve: u64, supply: u64) -> u64 {
    if base_reserve == 0 {
        return 0;
    }
    let numer = (quote_reserve as u128).saturating_mul(supply as u128);
    u64::try_from(numer / base_reserve as u128).unwrap_or(u64::MAX)
}

/// Requested `(base_amount, quote_amount)` from a CPMM swap instruction's
/// args, used for **failed** transactions (no `SwapEvent` is emitted).
///
/// Arg layout, two `u64`s after the 8-byte discriminator:
/// - `swap_base_input`:  amount_in @8, minimum_amount_out @16
/// - `swap_base_output`: max_amount_in @8, amount_out @16
///
/// `base_is_input` says which leg is the tracked mint. Returns `(0, 0)` for
/// truncated data.
pub fn cpmm_requested_amounts(data: &[u8], base_is_input: bool) -> (u64, u64) {
    let (Some(arg0), Some(arg1)) = (u64_at(data, 8), u64_at(data, 16)) else {
        return (0, 0);
    };
    // arg0 always describes the input leg, arg1 the output leg.
    if base_is_input {
        (arg0, arg1)
    } else {
        (arg1, arg0)
    }
}

/// Every CPMM `SwapEvent` in a transaction's log messages, in emission
/// (= execution) order. Other programs' `SwapEvent`s with the same
/// discriminator are rejected by the length guard in [`CpmmSwapEvent::parse`].
pub fn cpmm_swap_events_from_logs(log_messages: &[String]) -> Vec<CpmmSwapEvent> {
    log_messages
        .iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .filter_map(|data| CpmmSwapEvent::parse(&data))
        .collect()
}

/// Reconstruct a CPMM swap from the vault token-balance deltas when the
/// `SwapEvent` log is unavailable (validators cap log output at 10 KB, and
/// large aggregator routes emit the line past the cap). Reserves are the
/// vaults' post balances, which include accrued fees, so the resulting market
/// cap is a slight overestimate; amounts are exact. Returns `None` if either
/// vault has no pre/post balance rows or nothing moved.
pub fn cpmm_swap_from_vault_balances(
    meta: &TransactionStatusMeta,
    pool_id: &str,
    input_vault_index: usize,
    output_vault_index: usize,
    input_mint: &str,
    output_mint: &str,
) -> Option<CpmmSwapEvent> {
    fn amount(balances: &[TokenBalance], index: usize) -> Option<u64> {
        balances
            .iter()
            .find(|b| b.account_index as usize == index)
            .and_then(|b| b.ui_token_amount.as_ref())
            .and_then(|u| u.amount.parse::<u64>().ok())
    }
    let pre_in = amount(&meta.pre_token_balances, input_vault_index)?;
    let post_in = amount(&meta.post_token_balances, input_vault_index)?;
    let pre_out = amount(&meta.pre_token_balances, output_vault_index)?;
    let post_out = amount(&meta.post_token_balances, output_vault_index)?;
    let input_amount = post_in.checked_sub(pre_in)?;
    let output_amount = pre_out.checked_sub(post_out)?;
    if input_amount == 0 || output_amount == 0 {
        return None;
    }
    Some(CpmmSwapEvent {
        pool_id: pool_id.to_string(),
        input_vault_before: pre_in,
        output_vault_before: pre_out,
        input_amount,
        output_amount,
        input_transfer_fee: 0,
        output_transfer_fee: 0,
        base_input: true,
        input_mint: input_mint.to_string(),
        output_mint: output_mint.to_string(),
        trade_fee: 0,
        creator_fee: 0,
        creator_fee_on_input: false,
    })
}

/// Decimals of `mint` as reported by the first matching post (then pre)
/// token-balance row. `None` when no account of that mint was touched.
pub fn token_decimals_from_meta(meta: &TransactionStatusMeta, mint: &str) -> Option<u8> {
    meta.post_token_balances
        .iter()
        .chain(meta.pre_token_balances.iter())
        .find(|tb| tb.mint == mint)
        .and_then(|tb| tb.ui_token_amount.as_ref())
        .and_then(|u| u8::try_from(u.decimals).ok())
}

/// The canonical CPMM pool for a (config, mint pair):
/// `PDA("pool", amm_config, token_0_mint, token_1_mint)` with the mints in
/// ascending byte order. A stonk.fun migration creates exactly this pool
/// under [`super::STONK_CPMM_AMM_CONFIG`], so a swap whose `pool_state`
/// equals it for `(config, base, launch quote)` is provably the migration
/// pool without any per-pool state.
pub fn cpmm_canonical_pool(
    cpmm_program_id: &Pubkey,
    amm_config: &Pubkey,
    mint_a: &Pubkey,
    mint_b: &Pubkey,
) -> Pubkey {
    let (token_0, token_1) = if mint_a.to_bytes() < mint_b.to_bytes() {
        (mint_a, mint_b)
    } else {
        (mint_b, mint_a)
    };
    Pubkey::find_program_address(
        &[
            CPMM_POOL_SEED,
            amm_config.as_ref(),
            token_0.as_ref(),
            token_1.as_ref(),
        ],
        cpmm_program_id,
    )
    .0
}

/// Resolve an instruction account as a `Pubkey`.
pub fn instruction_account_pubkey(
    instr: &CompiledInstruction,
    full_accounts: &[Vec<u8>],
    position: usize,
) -> Option<Pubkey> {
    let account_index = *instr.accounts.get(position)? as usize;
    let bytes: [u8; 32] = full_accounts
        .get(account_index)?
        .as_slice()
        .try_into()
        .ok()?;
    Some(Pubkey::new_from_array(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::{RAYDIUM_CPMM_PROGRAM_ID, STONK_CPMM_AMM_CONFIG};
    use serde_json::Value;
    use std::str::FromStr;
    use yellowstone_grpc_proto::prelude::UiTokenAmount;

    const TX_CPMM_SWAP: &str =
        include_str!("../../fixtures/stonk/tx_cpmm_swap_base_input_2cY4Pc3j.json");
    const BASE_MINT: &str = "HrSjY8WztUJFavB2Joz5ojFLpMhfpNmxMyL57zUENcDg";
    const QUOTE_MINT: &str = "DoGEV7LASBkQbibMc5k5vKnTZoMg423GpJ5QtJEGfm7R";
    const POOL: &str = "DX8ykUKbMWduHgfQ5JnjfEec1oWaVbfuVRef6r1HRqHt";
    const SUPPLY: u64 = 1_000_000_000_000_000;

    fn tx() -> Value {
        let v: Value = serde_json::from_str(TX_CPMM_SWAP).unwrap();
        match v.get("result") {
            Some(r) => r.clone(),
            None => v,
        }
    }

    fn log_lines(tx: &Value) -> Vec<String> {
        tx["meta"]["logMessages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn cpmm_swap_event_from_fixture() {
        let tx = tx();
        let events = cpmm_swap_events_from_logs(&log_lines(&tx));
        // Two `SwapEvent`-discriminated payloads are in the logs: the CPMM one
        // and the Raydium CLMM one (213-byte body) from the aggregator's
        // second hop. Only the CPMM one parses.
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.pool_id, POOL);
        assert_eq!(ev.input_vault_before, 265_393_413_454_563);
        assert_eq!(ev.output_vault_before, 8_044_251_865_337);
        assert_eq!(ev.input_amount, 4_098_714_943_283);
        assert_eq!(ev.output_amount, 120_823_636_524);
        assert_eq!(ev.input_transfer_fee, 41_401_161_044);
        assert_eq!(ev.output_transfer_fee, 0);
        assert!(ev.base_input);
        assert_eq!(ev.input_mint, BASE_MINT);
        assert_eq!(ev.output_mint, QUOTE_MINT);
        assert_eq!(ev.trade_fee, 10_246_787_359);
        assert_eq!(ev.creator_fee, 1_220_440_773);
        assert!(!ev.creator_fee_on_input);

        let side = cpmm_resolve_side(ev, BASE_MINT).expect("tracked mint is the input");
        assert!(!side.is_buy);
        assert_eq!(side.base_amount, 4_098_714_943_283);
        assert_eq!(side.quote_amount, 120_823_636_524);
        assert_eq!(side.base_reserve_after, 269_492_128_397_846);
        assert_eq!(side.quote_reserve_after, 7_923_428_228_813);
        assert_eq!(side.base_transfer_fee, 41_401_161_044);

        let expected = 7_923_428_228_813u128 * SUPPLY as u128 / 269_492_128_397_846u128;
        let mc = cpmm_market_cap(side.quote_reserve_after, side.base_reserve_after, SUPPLY);
        assert_eq!(mc, expected as u64);
        assert!((29_300_000_000_000..=29_500_000_000_000).contains(&mc));

        assert!(cpmm_resolve_side(ev, "nope").is_none());
        let mirrored = cpmm_resolve_side(ev, QUOTE_MINT).unwrap();
        assert!(mirrored.is_buy);
        assert_eq!(mirrored.base_amount, 120_823_636_524);
        assert_eq!(mirrored.quote_amount, 4_098_714_943_283);
    }

    #[test]
    fn swap_event_length_guard() {
        let mut short = CPMM_SWAP_EVENT_DISC.to_vec();
        short.extend(std::iter::repeat_n(0u8, CPMM_SWAP_EVENT_BODY_LEN - 1));
        assert!(CpmmSwapEvent::parse(&short).is_none());
        let mut long = CPMM_SWAP_EVENT_DISC.to_vec();
        long.extend(std::iter::repeat_n(0u8, CPMM_SWAP_EVENT_BODY_LEN + 1));
        assert!(CpmmSwapEvent::parse(&long).is_none());
        let mut wrong_disc = vec![0u8; 8];
        wrong_disc.extend(std::iter::repeat_n(0u8, CPMM_SWAP_EVENT_BODY_LEN));
        assert!(CpmmSwapEvent::parse(&wrong_disc).is_none());
        let mut ok = CPMM_SWAP_EVENT_DISC.to_vec();
        ok.extend(std::iter::repeat_n(0u8, CPMM_SWAP_EVENT_BODY_LEN));
        assert!(CpmmSwapEvent::parse(&ok).is_some());
        assert!(CpmmSwapEvent::parse(&[]).is_none());
    }

    #[test]
    fn market_cap_edge_cases() {
        assert_eq!(cpmm_market_cap(1, 0, SUPPLY), 0);
        assert_eq!(cpmm_market_cap(u64::MAX, 1, u64::MAX), u64::MAX);
        assert_eq!(cpmm_market_cap(10, 5, 100), 200);
    }

    #[test]
    fn requested_amounts_follow_input_output_legs() {
        let mut data = vec![0u8; 8];
        data.extend_from_slice(&11u64.to_le_bytes());
        data.extend_from_slice(&22u64.to_le_bytes());
        // base is the input leg (sell): base = arg0, quote = arg1
        assert_eq!(cpmm_requested_amounts(&data, true), (11, 22));
        // base is the output leg (buy): base = arg1, quote = arg0
        assert_eq!(cpmm_requested_amounts(&data, false), (22, 11));
        assert_eq!(cpmm_requested_amounts(&data[..20], true), (0, 0));
    }

    /// The migration pool is the plain CPMM pool PDA for
    /// (Stonk amm_config, sorted mint pair). Checked against the fixture pool
    /// and against pools recorded by strategy-data in `alt_mints.cpmm_pool`.
    #[test]
    fn canonical_pool_pda_matches_live_migration_pools() {
        let program = Pubkey::from_str(RAYDIUM_CPMM_PROGRAM_ID).unwrap();
        let config = Pubkey::from_str(STONK_CPMM_AMM_CONFIG).unwrap();
        let cases = [
            (BASE_MINT, QUOTE_MINT, POOL),
            (
                "AsSeYCDN6LyHKCGuYvTP2ngd2i9w6DUyVnGguFSZqYEA",
                "Pren1FvFX6J3E4kXhJuCiAD5aDmGEb7qJRncwA8Lkhw",
                "8ZyaQfrYJoJN5wDQDWm7qPrbik3LFH2gsfnRrHrLC9cf",
            ),
            (
                "5KjeZ4JzTJWyHcKwjcPimApPvY3ASmoJKMGDQat1dFUx",
                "So11111111111111111111111111111111111111112",
                "9iHdzvS2C6uAhPvvSQ4Zp4He72LYZFnZx8Z4jV5Zy2ZW",
            ),
            (
                "6r1Rdfx6WRmhmfhV7rpbZn1hq7gwS9JveShR5SrTbQUr",
                "Xsf9mBktVB9BSU5kf4nHxPq5hCBJ2j2ui3ecFGxPRGc",
                "F2hCQqTXYhtN2sP8i9Bn3eeqFLYdY6eFoBroT8FcLgvR",
            ),
        ];
        for (base, quote, pool) in cases {
            let base = Pubkey::from_str(base).unwrap();
            let quote = Pubkey::from_str(quote).unwrap();
            let expected = Pubkey::from_str(pool).unwrap();
            assert_eq!(cpmm_canonical_pool(&program, &config, &base, &quote), expected);
            // Order-insensitive.
            assert_eq!(cpmm_canonical_pool(&program, &config, &quote, &base), expected);
        }
    }

    #[test]
    fn vault_balance_fallback_reconstructs_amounts() {
        let row = |idx: u32, mint: &str, amount: &str| TokenBalance {
            account_index: idx,
            mint: mint.to_string(),
            owner: String::new(),
            program_id: String::new(),
            ui_token_amount: Some(UiTokenAmount {
                ui_amount: 0.0,
                decimals: 6,
                amount: amount.to_string(),
                ui_amount_string: amount.to_string(),
            }),
        };
        let meta = TransactionStatusMeta {
            pre_token_balances: vec![row(4, BASE_MINT, "1000"), row(5, QUOTE_MINT, "500")],
            post_token_balances: vec![row(4, BASE_MINT, "1100"), row(5, QUOTE_MINT, "460")],
            ..Default::default()
        };
        let ev = cpmm_swap_from_vault_balances(&meta, POOL, 4, 5, BASE_MINT, QUOTE_MINT).unwrap();
        assert_eq!(ev.input_amount, 100);
        assert_eq!(ev.output_amount, 40);
        assert_eq!(ev.input_vault_before, 1000);
        assert_eq!(ev.output_vault_before, 500);
        assert_eq!(token_decimals_from_meta(&meta, BASE_MINT), Some(6));
        assert_eq!(token_decimals_from_meta(&meta, "nope"), None);
        // Nothing moved / missing rows → None.
        assert!(cpmm_swap_from_vault_balances(&meta, POOL, 4, 9, BASE_MINT, QUOTE_MINT).is_none());
    }
}
