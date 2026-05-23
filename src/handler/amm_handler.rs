use anyhow::Result;
use chrono::Utc;
use dashmap::DashSet;
use log::{error, info, warn};
use solana_program::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use yellowstone_grpc_proto::geyser::SubscribeUpdateTransactionInfo;

use crate::database::TradeRecord;
use crate::utils::{
    AMM_BUY_DISCRIMINATOR, AMM_SELL_DISCRIMINATOR, BUY_EXACT_IN_DISCRIMINATOR,
    PUMP_SWAP_BUY_EVENT_DISC, PUMP_SWAP_PROGRAM_ID, PUMP_SWAP_SELL_EVENT_DISC,
    extract_coin_creator_fee, extract_pool_reserves_from_data, extract_quote_volume,
    extract_transaction_amounts, extract_transaction_fees, find_event_data,
    get_market_cap_in_quote, get_program_instructions, get_user, pump_swap_quote_currency,
    resolve_pump_swap_memecoin,
};

/// Per-process running count of trades emitted (across all txs). Used by readonly
/// validation logs so the operator can quickly see throughput.
static TRADE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates the gRPC handler closure that parses Pump Swap AMM transactions,
/// filters by whitelisted mints, and sends trade records to the batch inserter.
///
/// `readonly`: when true, the whitelist filter is bypassed and every parsed
/// trade is logged at INFO level instead of sent to the DB-write channel.
pub fn create_amm_handler(
    whitelist: Arc<DashSet<String>>,
    sender: mpsc::Sender<TradeRecord>,
    readonly: bool,
) -> impl Fn(SubscribeUpdateTransactionInfo, u64) -> Result<()> + Clone + Send + Sync + 'static {
    let handler = Arc::new(move |tx_data: SubscribeUpdateTransactionInfo, slot: u64| {
        let whitelist = whitelist.clone();
        let sender = sender.clone();
        tokio::spawn(async move {
            if let Err(e) = process_pump_swap_tx(tx_data, slot, &whitelist, &sender, readonly) {
                error!("Error processing AMM transaction: {:?}", e);
            }
        });
        Ok(())
    });

    move |tx_data, slot| handler(tx_data, slot)
}

/// Parse a Pump Swap transaction, extract every trade instruction (top-level + CPI),
/// check whitelist, and send a record per trade to the channel.
///
/// Detection is discriminator-based on the instruction `data` (canonical Anchor
/// approach) — independent of log truncation and stable for failed txs. Event
/// payloads are read from inner-instruction `emit_cpi!` data (not "Program data:"
/// logs) so multi-trade transactions are attributed correctly via stack_height
/// bounds in `find_event_data`.
fn process_pump_swap_tx(
    tx_data: SubscribeUpdateTransactionInfo,
    slot: u64,
    whitelist: &DashSet<String>,
    sender: &mpsc::Sender<TradeRecord>,
    readonly: bool,
) -> Result<()> {
    // Skip failed transactions
    if tx_data.meta.as_ref().is_some_and(|m| m.err.is_some()) {
        return Ok(());
    }

    let program_id = Pubkey::from_str(PUMP_SWAP_PROGRAM_ID)?;

    let msg = tx_data
        .transaction
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Transaction missing"))?
        .message
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Message missing"))?;

    let account_keys = &msg.account_keys;

    let meta = tx_data
        .meta
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Meta missing"))?;

    // Build full account list including loaded addresses (ALT lookups).
    let mut full_accounts = account_keys.clone();
    full_accounts.extend(meta.loaded_writable_addresses.iter().cloned());
    full_accounts.extend(meta.loaded_readonly_addresses.iter().cloned());

    let all_instrs = get_program_instructions(msg, meta, &full_accounts, &program_id);

    let tx_signature = bs58::encode(&tx_data.signature).into_string();
    let now = Utc::now();
    let user = get_user(&full_accounts);

    // Fees are tx-level (priority fee on the compute budget ix; tip on a single
    // transfer), so extract once and replicate to every trade record emitted
    // for this tx.
    let (priority_fee, transfer_tip, tip_provider) =
        extract_transaction_fees(msg, meta, &full_accounts);

    for (ix_index, (instr, parent_outer_idx, start_inner_pos)) in all_instrs.iter().enumerate() {
        if instr.data.len() < 8 {
            continue;
        }
        let disc = &instr.data[..8];

        let is_buy = disc == AMM_BUY_DISCRIMINATOR.as_slice()
            || disc == BUY_EXACT_IN_DISCRIMINATOR.as_slice();
        let is_sell = disc == AMM_SELL_DISCRIMINATOR.as_slice();
        if !is_buy && !is_sell {
            continue;
        }

        // Accept SOL and USDC quoted pools; reject arbitrary quote mints.
        // Existing column names are kept for compatibility, but `sol_amount`
        // and `market_cap` are native quote units when `is_usdc = true`.
        let quote_currency = match pump_swap_quote_currency(instr, &full_accounts) {
            Some(q) => q,
            None => continue,
        };
        let is_usdc = quote_currency.is_usdc();

        // Resolve memecoin mint at IDL position 3, skipping scam pools where base==WSOL.
        let mint = match resolve_pump_swap_memecoin(instr, &full_accounts) {
            Some(m) => m,
            None => continue,
        };

        // Whitelist filter — must come after mint resolution.
        // Bypassed in readonly validation mode so every AMM trade is observable.
        if !readonly && !whitelist.contains(&mint) {
            continue;
        }

        // Pull the matching event payload from inner instructions (Anchor self-CPI).
        let event_disc = if is_buy {
            &PUMP_SWAP_BUY_EVENT_DISC
        } else {
            &PUMP_SWAP_SELL_EVENT_DISC
        };
        let event_data =
            match find_event_data(meta, *parent_outer_idx, *start_inner_pos, event_disc) {
                Some(d) => d,
                None => continue,
            };

        // Non-canonical pools (e.g. clones of the program with a forged pool
        // PDA) emit a zero `coin_creator_fee_basis_points`. Canonical pump_amm
        // pools always carry a non-zero value, so this is the cheapest sentinel
        // for filtering them out before any DB write.
        if let Some(0) = extract_coin_creator_fee(event_data) {
            continue;
        }

        let (Some(base_reserves), Some(quote_reserves)) =
            extract_pool_reserves_from_data(event_data)
        else {
            continue;
        };

        let (buy_vol, sell_vol) = extract_quote_volume(event_data);
        let quote_volume = if is_buy {
            buy_vol.unwrap_or(0)
        } else {
            sell_vol.unwrap_or(0)
        };

        let token_amount = extract_transaction_amounts(event_data).unwrap_or(0);

        let market_cap = get_market_cap_in_quote(
            base_reserves,
            quote_reserves,
            token_amount,
            quote_volume,
            is_buy,
            quote_currency,
        );

        let record = TradeRecord {
            tx_signature: tx_signature.clone(),
            ix_index: ix_index as i32,
            mint_address: mint.clone(),
            user_pubkey: user.clone(),
            is_buy,
            token_amount: token_amount as i64,
            sol_amount: quote_volume as i64,
            market_cap: Some(market_cap as i64),
            is_usdc,
            slot: slot as i64,
            created_at: now,
            priority_fee: priority_fee.map(|v| v as i64),
            transfer_tip: transfer_tip.map(|v| v as i64),
            tip_provider: tip_provider.clone(),
        };

        if readonly {
            // Validation log — every field that ends up in the DB, plus the
            // human-readable SOL conversions, so a tx can be looked up via RPC
            // and cross-checked.
            let n = TRADE_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
            let quote_base_unit = quote_currency.base_unit();
            let quote_amount_f = quote_volume as f64 / quote_base_unit;
            let token_amount_f = token_amount as f64 / 1_000_000.0;
            let market_cap_quote = market_cap as f64 / quote_base_unit;
            info!(
                "[TRADE #{n}] slot={slot} sig={sig} ix={ix} {side} mint={mint} user={user} \
                 quote={quote:.6} quote_ccy={ccy} is_usdc={is_usdc} tok={tok:.3} \
                 mc={mc:.3} {ccy} base_res={br} quote_res={qr} prio={prio:?} \
                 tip={tip:?} provider={prov:?}",
                n = n,
                slot = slot,
                sig = tx_signature,
                ix = ix_index,
                side = if is_buy { "BUY " } else { "SELL" },
                mint = mint,
                user = user,
                quote = quote_amount_f,
                ccy = quote_currency.label(),
                is_usdc = is_usdc,
                tok = token_amount_f,
                mc = market_cap_quote,
                br = base_reserves,
                qr = quote_reserves,
                prio = priority_fee,
                tip = transfer_tip,
                prov = tip_provider,
            );
        } else if let Err(e) = sender.try_send(record) {
            warn!("Trade channel full or closed, dropping trade: {:?}", e);
        }
    }

    Ok(())
}
