use anyhow::Result;
use chrono::{DateTime, Utc};
use dashmap::DashSet;
use log::{info, warn};
use solana_program::pubkey::Pubkey;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use yellowstone_grpc_proto::geyser::SubscribeUpdateTransactionInfo;
use yellowstone_grpc_proto::prelude::{CompiledInstruction, TransactionStatusMeta};

use crate::database::TradeRecord;
use crate::utils::{
    AMM_MULTI_HOP_SWAP_DISCRIMINATOR, PUMP_PROGRAM_ID, PUMP_SWAP_BOOST_BUY_EVENT_DISC,
    PUMP_SWAP_BUY_EVENT_DISC, PUMP_SWAP_MULTI_HOP_USER_IX_POS, PUMP_SWAP_PROGRAM_ID,
    PUMP_SWAP_SELL_EVENT_DISC, PumpSwapHop, QuoteCurrency, WSOL_MINT, canonical_pump_swap_pool,
    direct_child_events, effective_quote_reserves, extract_boost_buy_event,
    extract_pool_reserves_from_data, extract_pump_swap_event_pool,
    extract_pump_swap_requested_amounts, extract_pump_swap_virtual_quote_reserves,
    extract_quote_volume, extract_transaction_amounts, extract_transaction_fees, find_event_data,
    get_market_cap_from_reserves, get_program_instructions, multi_hop_input_mint,
    multi_hop_is_routed, multi_hop_sides, multi_hop_swap_hops, pump_swap_trade_ix,
    quote_currency_of, resolve_instruction_account, resolve_pump_swap_memecoin,
    resolve_pump_swap_pool, resolve_pump_swap_quote_mint,
};

/// Per-process running count of trades emitted (across all txs). Used by readonly
/// validation logs so the operator can quickly see throughput.
static TRADE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `instruction_type` of a `multi_hop_swap` hop on the coin the user actually
/// buys or sells (the route's end coin).
pub const MULTI_HOP_INSTRUCTION_TYPE: &str = "multi_hop_swap";
/// `instruction_type` of a `multi_hop_swap` middle hop: a real pool trade
/// (reserves and price move) on a coin that never reaches the user's wallet,
/// so `user_pubkey` is the route's user, not a holder of the coin.
pub const MULTI_HOP_ROUTED_INSTRUCTION_TYPE: &str = "multi_hop_swap_routed";

/// Creates the gRPC handler closure that parses Pump Swap AMM transactions,
/// sends every qualifying trade record to the batch inserter.
///
/// `readonly`: when true, every parsed trade is logged at INFO level instead
/// of sent to the DB-write channel.
pub fn create_amm_handler(
    whitelist: Arc<DashSet<String>>,
    sender: mpsc::Sender<TradeRecord>,
    readonly: bool,
) -> impl Fn(SubscribeUpdateTransactionInfo, u64) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
+ Clone
+ Send
+ Sync
+ 'static {
    move |tx_data: SubscribeUpdateTransactionInfo, slot: u64| {
        let whitelist = whitelist.clone();
        let sender = sender.clone();
        Box::pin(
            async move { process_pump_swap_tx(tx_data, slot, &whitelist, &sender, readonly).await },
        )
    }
}

/// Tx-level fields replicated onto every trade record of one transaction.
struct TxContext<'a> {
    tx_signature: String,
    slot: u64,
    now: DateTime<Utc>,
    success: bool,
    priority_fee: Option<u64>,
    transfer_tip: Option<u64>,
    tip_provider: Option<String>,
    compute_units_consumed: Option<i64>,
    lamports_per_compute_unit: Option<f64>,
    whitelist: &'a DashSet<String>,
    sender: &'a mpsc::Sender<TradeRecord>,
    readonly: bool,
    pump_program_id: Pubkey,
    pump_swap_program_id: Pubkey,
}

/// One trade on one pool, ready to be written.
struct Fill {
    pool_address: String,
    mint: String,
    quote_mint: String,
    quote_currency: QuoteCurrency,
    instruction_type: &'static str,
    is_cpi: bool,
    user: String,
    is_buy: bool,
    token_amount: u64,
    quote_volume: u64,
    market_cap: Option<u64>,
    base_reserves: u64,
    quote_reserves: u64,
    amount_source: &'static str,
}

impl TxContext<'_> {
    /// The intentional pool filters, shared by every trade path: a non-WSOL
    /// base, a whitelisted base (unless readonly), and the canonical
    /// migration pool. Any quote mint is kept (SOL, USDC, stock tokens, pump
    /// coins, ...). Returns the pool's quote currency if eligible.
    fn eligible(&self, pool: &Pubkey, base: &Pubkey, quote: &Pubkey) -> Option<QuoteCurrency> {
        let mint = base.to_string();
        if mint == WSOL_MINT {
            return None;
        }
        let quote_currency = quote_currency_of(&quote.to_string())?;
        if !self.readonly && !self.whitelist.contains(&mint) {
            return None;
        }
        let canonical = canonical_pump_swap_pool(
            base,
            quote,
            &self.pump_program_id,
            &self.pump_swap_program_id,
        );
        (*pool == canonical).then_some(quote_currency)
    }

    async fn emit(&self, fill: Fill, trade_index: i32) {
        let Fill {
            pool_address,
            mint,
            quote_mint,
            quote_currency,
            instruction_type,
            is_cpi,
            user,
            is_buy,
            token_amount,
            quote_volume,
            market_cap,
            base_reserves,
            quote_reserves,
            amount_source,
        } = fill;
        let is_usdc = quote_currency.is_usdc();
        let record = TradeRecord {
            tx_signature: self.tx_signature.clone(),
            ix_index: trade_index,
            pool_address,
            mint_address: mint.clone(),
            quote_mint_address: quote_mint,
            instruction_type: instruction_type.to_string(),
            amount_source: amount_source.to_string(),
            is_cpi,
            user_pubkey: user.clone(),
            is_buy,
            token_amount,
            sol_amount: quote_volume,
            market_cap,
            is_usdc,
            success: self.success,
            slot: self.slot as i64,
            created_at: self.now,
            priority_fee: self.priority_fee.map(|v| v as i64),
            transfer_tip: self.transfer_tip.map(|v| v as i64),
            tip_provider: self.tip_provider.clone(),
            compute_units_consumed: self.compute_units_consumed,
            lamports_per_compute_unit: self.lamports_per_compute_unit,
        };

        if self.readonly {
            // Validation log — every field that ends up in the DB, plus the
            // human-readable SOL conversions, so a tx can be looked up via RPC
            // and cross-checked.
            let n = TRADE_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
            let quote_base_unit = quote_currency.base_unit();
            let quote_amount_f = quote_volume as f64 / quote_base_unit;
            let token_amount_f = token_amount as f64 / 1_000_000.0;
            let market_cap_quote = market_cap.map(|m| m as f64 / quote_base_unit);
            info!(
                "[TRADE #{n}] slot={slot} sig={sig} ix={ix} {side} mint={mint} user={user} \
                 kind={kind} route={route} success={succ} quote={quote:.6} quote_ccy={ccy} is_usdc={is_usdc} tok={tok:.3} \
                 mc={mc:?} base_res={br} quote_res={qr} prio={prio:?} \
                 tip={tip:?} provider={prov:?} cu={cu:?} lpcu={lpcu:?}",
                n = n,
                slot = self.slot,
                sig = self.tx_signature,
                ix = trade_index,
                side = if is_buy { "BUY " } else { "SELL" },
                mint = mint,
                user = user,
                kind = instruction_type,
                route = if is_cpi { "cpi" } else { "direct" },
                succ = self.success,
                quote = quote_amount_f,
                ccy = quote_currency.label(),
                is_usdc = is_usdc,
                tok = token_amount_f,
                mc = market_cap_quote,
                br = base_reserves,
                qr = quote_reserves,
                prio = self.priority_fee,
                tip = self.transfer_tip,
                prov = self.tip_provider,
                cu = self.compute_units_consumed,
                lpcu = self.lamports_per_compute_unit,
            );
        } else if let Err(e) = self.sender.send(record).await {
            warn!("Trade channel closed; unable to persist trade: {:?}", e);
        }
    }
}

/// Executed `(token_amount, quote_volume, market_cap, base_reserves,
/// quote_reserves)` from a BuyEvent / SellEvent / BoostBuyAndBurnEvent
/// payload. `None` if the payload is truncated.
fn event_amounts(
    event_data: &[u8],
    event_disc: &[u8; 8],
    is_buy: bool,
    quote_currency: QuoteCurrency,
) -> Option<(u64, u64, Option<u64>, u64, u64)> {
    if event_disc == &PUMP_SWAP_BOOST_BUY_EVENT_DISC {
        let (token_amount, quote_volume, base_reserves, quote_reserves) =
            extract_boost_buy_event(event_data)?;
        let market_cap =
            get_market_cap_from_reserves(base_reserves, quote_reserves, quote_currency);
        return Some((
            token_amount,
            quote_volume,
            Some(market_cap),
            base_reserves,
            quote_reserves,
        ));
    }
    let (base_reserves, raw_quote_reserves) = extract_pool_reserves_from_data(event_data);
    let (buy_vol, sell_vol) = extract_quote_volume(event_data);
    let quote_volume = if is_buy { buy_vol } else { sell_vol }?;
    let token_amount = extract_transaction_amounts(event_data)?;
    let base_reserves = base_reserves?;
    // Price against effective quote reserves, not the raw vault balance —
    // matches what the boost path above already reports. Absent on
    // pre-2026-07-15 payloads, where effective == raw.
    let quote_reserves = effective_quote_reserves(
        raw_quote_reserves?,
        extract_pump_swap_virtual_quote_reserves(event_data, is_buy),
    );
    // BuyEvent/SellEvent pool reserves are the post-trade values. Applying
    // the trade delta again creates a phantom price one fill ahead.
    let market_cap = get_market_cap_from_reserves(base_reserves, quote_reserves, quote_currency);
    Some((
        token_amount,
        quote_volume,
        Some(market_cap),
        base_reserves,
        quote_reserves,
    ))
}

/// Parse a Pump Swap transaction, extract every trade instruction (top-level + CPI),
/// and send a record per trade to the channel.
///
/// Detection is discriminator-based on the instruction `data` (canonical Anchor
/// approach) — independent of log truncation and stable for failed txs. Event
/// payloads are read from inner-instruction `emit_cpi!` data (not "Program data:"
/// logs) so multi-trade transactions are attributed correctly via stack_height
/// bounds in `find_event_data`.
pub(crate) async fn process_pump_swap_tx(
    tx_data: SubscribeUpdateTransactionInfo,
    slot: u64,
    whitelist: &DashSet<String>,
    sender: &mpsc::Sender<TradeRecord>,
    readonly: bool,
) -> Result<()> {
    // Whether the transaction landed on-chain. Failed txs are still recorded
    // (with `success = false`) so we capture every attempted trade on a mint,
    // not just the ones that executed. A failed tx carries no BuyEvent/SellEvent
    // self-CPI, so for those we record the *requested* amounts parsed from the
    // instruction args rather than executed amounts/reserves/market cap.
    let tx_succeeded = tx_data.meta.as_ref().is_none_or(|m| m.err.is_none());

    let program_id = Pubkey::from_str(PUMP_SWAP_PROGRAM_ID)?;
    let pump_program_id = Pubkey::from_str(PUMP_PROGRAM_ID)?;

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
    if all_instrs.is_empty() {
        return Ok(());
    }

    // Fees are tx-level (priority fee on the compute budget ix; tip on a single
    // transfer), so extract once and replicate to every trade record emitted
    // for this tx.
    let (priority_fee, transfer_tip, tip_provider) =
        extract_transaction_fees(msg, meta, &full_accounts);

    // Compute units actually consumed (tx-level, from gRPC meta) and the priority
    // fee bid converted from microlamports/CU to lamports/CU. Both replicate to
    // every trade record for this tx.
    let compute_units_consumed = meta.compute_units_consumed.map(|v| v as i64);
    let lamports_per_compute_unit = priority_fee.map(|p| p as f64 / 1_000_000.0);

    let ctx = TxContext {
        tx_signature: bs58::encode(&tx_data.signature).into_string(),
        slot,
        now: Utc::now(),
        success: tx_succeeded,
        priority_fee,
        transfer_tip,
        tip_provider,
        compute_units_consumed,
        lamports_per_compute_unit,
        whitelist,
        sender,
        readonly,
        pump_program_id,
        pump_swap_program_id: program_id,
    };

    let mut trade_index = 0i32;
    for (instr, parent_outer_idx, start_inner_pos) in &all_instrs {
        if instr.data.len() < 8 {
            continue;
        }
        let disc = &instr.data[..8];

        if disc == AMM_MULTI_HOP_SWAP_DISCRIMINATOR.as_slice() {
            for fill in multi_hop_fills(
                &ctx,
                instr,
                meta,
                &full_accounts,
                *parent_outer_idx,
                *start_inner_pos,
            ) {
                ctx.emit(fill, trade_index).await;
                trade_index += 1;
            }
            continue;
        }

        let Some(kind) = pump_swap_trade_ix(disc) else {
            continue;
        };

        let Some(mint) = resolve_pump_swap_memecoin(instr, &full_accounts) else {
            continue;
        };
        let Some(quote_mint) = resolve_pump_swap_quote_mint(instr, &full_accounts) else {
            continue;
        };
        let Some(pool_address) = resolve_pump_swap_pool(instr, &full_accounts) else {
            continue;
        };
        let (Ok(base), Ok(quote), Ok(pool)) = (
            Pubkey::from_str(&mint),
            Pubkey::from_str(&quote_mint),
            Pubkey::from_str(&pool_address),
        ) else {
            continue;
        };
        let Some(quote_currency) = ctx.eligible(&pool, &base, &quote) else {
            continue;
        };
        let user = resolve_instruction_account(instr, &full_accounts, 1)
            .unwrap_or_else(|| "unknown".to_string());

        // Successful txs: pull executed amounts/reserves from the BuyEvent/
        // SellEvent self-CPI and compute market cap. Failed txs emit no event,
        // so record the *requested* amounts from the instruction args and leave
        // market cap NULL.
        let executed = tx_succeeded
            .then(|| {
                find_event_data(
                    meta,
                    *parent_outer_idx,
                    *start_inner_pos,
                    instr.program_id_index,
                    kind.event_disc,
                )
            })
            .flatten()
            .and_then(|event_data| {
                event_amounts(event_data, kind.event_disc, kind.is_buy, quote_currency)
            });
        let (token_amount, quote_volume, market_cap, base_reserves, quote_reserves, amount_source) =
            match executed {
                Some((token, quote, mc, base_res, quote_res)) => {
                    (token, quote, mc, base_res, quote_res, "event")
                }
                None => {
                    let (token, quote) =
                        extract_pump_swap_requested_amounts(&instr.data, kind.is_exact_in);
                    (token, quote, None, 0, 0, "instruction_request")
                }
            };

        ctx.emit(
            Fill {
                pool_address,
                mint,
                quote_mint,
                quote_currency,
                instruction_type: kind.name,
                is_cpi: *start_inner_pos > 0,
                user,
                is_buy: kind.is_buy,
                token_amount,
                quote_volume,
                market_cap,
                base_reserves,
                quote_reserves,
                amount_source,
            },
            trade_index,
        )
        .await;
        trade_index += 1;
    }

    Ok(())
}

/// The recordable pool hops of one `multi_hop_swap`, in route order.
///
/// Successful route: one fill per BuyEvent / SellEvent the instruction emits,
/// matched to its hop by the event's `pool` (events come in route order, so
/// the search resumes after the previous match). Curve hops emit pump
/// `TradeEvent`s, not pool events, and are skipped — this collector records
/// PumpSwap pool trades only.
///
/// Failed route: no events, so each hop's side comes from the route's input
/// mint ([`multi_hop_sides`]); the requested `amount_in` goes on the first
/// hop's input side and `min_amount_out` on the last hop's output side.
///
/// A middle hop is tagged [`MULTI_HOP_ROUTED_INSTRUCTION_TYPE`].
fn multi_hop_fills(
    ctx: &TxContext<'_>,
    instr: &CompiledInstruction,
    meta: &TransactionStatusMeta,
    full_accounts: &[Vec<u8>],
    parent_outer_idx: usize,
    start_inner_pos: usize,
) -> Vec<Fill> {
    let hops = multi_hop_swap_hops(instr, full_accounts);
    let user = resolve_instruction_account(instr, full_accounts, PUMP_SWAP_MULTI_HOP_USER_IX_POS)
        .unwrap_or_else(|| "unknown".to_string());
    let is_cpi = start_inner_pos > 0;
    let fill = |hop: &PumpSwapHop,
                quote_currency: QuoteCurrency,
                is_buy: bool,
                amounts: (u64, u64, Option<u64>, u64, u64),
                amount_source: &'static str| {
        let (token_amount, quote_volume, market_cap, base_reserves, quote_reserves) = amounts;
        Fill {
            pool_address: hop.pool.to_string(),
            mint: hop.base_mint.to_string(),
            quote_mint: hop.quote_mint.to_string(),
            quote_currency,
            instruction_type: if multi_hop_is_routed(&hops, &hop.base_mint, is_buy) {
                MULTI_HOP_ROUTED_INSTRUCTION_TYPE
            } else {
                MULTI_HOP_INSTRUCTION_TYPE
            },
            is_cpi,
            user: user.clone(),
            is_buy,
            token_amount,
            quote_volume,
            market_cap,
            base_reserves,
            quote_reserves,
            amount_source,
        }
    };

    let mut fills = Vec::new();
    if ctx.success {
        let mut next_hop = 0;
        for event_data in direct_child_events(
            meta,
            parent_outer_idx,
            start_inner_pos,
            instr.program_id_index,
        ) {
            let disc: &[u8; 8] = event_data[..8].try_into().unwrap();
            let is_buy = if disc == &PUMP_SWAP_BUY_EVENT_DISC {
                true
            } else if disc == &PUMP_SWAP_SELL_EVENT_DISC {
                false
            } else {
                continue;
            };
            let Some(pool) = extract_pump_swap_event_pool(event_data) else {
                continue;
            };
            let Some(offset) = hops[next_hop..].iter().position(|h| h.pool == pool) else {
                warn!(
                    "multi_hop_swap pool event {pool} matches no hop account group in {}",
                    ctx.tx_signature
                );
                continue;
            };
            let hop = &hops[next_hop + offset];
            next_hop += offset + 1;
            let Some(quote_currency) = ctx.eligible(&hop.pool, &hop.base_mint, &hop.quote_mint)
            else {
                continue;
            };
            let Some(amounts) = event_amounts(event_data, disc, is_buy, quote_currency) else {
                continue;
            };
            fills.push(fill(hop, quote_currency, is_buy, amounts, "event"));
        }
        return fills;
    }

    let Some(sides) = multi_hop_input_mint(instr, meta, &hops)
        .and_then(|input_mint| multi_hop_sides(&hops, &input_mint))
    else {
        return fills;
    };
    // `amount_in`, `min_amount_out` — same (input, output) shape as the
    // single-pool args, so read them as a non-exact-in pair.
    let (amount_in, min_amount_out) = extract_pump_swap_requested_amounts(&instr.data, false);
    let last = hops.len().saturating_sub(1);
    for (i, (hop, is_buy)) in hops.iter().zip(sides).enumerate() {
        let Some(quote_currency) = ctx.eligible(&hop.pool, &hop.base_mint, &hop.quote_mint) else {
            continue;
        };
        let (mut token_amount, mut quote_volume) = (0, 0);
        if i == 0 {
            *(if is_buy {
                &mut quote_volume
            } else {
                &mut token_amount
            }) = amount_in;
        }
        if i == last {
            *(if is_buy {
                &mut token_amount
            } else {
                &mut quote_volume
            }) = min_amount_out;
        }
        fills.push(fill(
            hop,
            quote_currency,
            is_buy,
            (token_amount, quote_volume, None, 0, 0),
            "instruction_request",
        ));
    }
    fills
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::{
        AMM_BUY_DISCRIMINATOR, AMM_BUY_EXACT_QUOTE_IN_V2_DISCRIMINATOR, AMM_BUY_V2_DISCRIMINATOR,
        AMM_SELL_DISCRIMINATOR, AMM_SELL_V2_DISCRIMINATOR, BOOST_BUY_AND_BURN_DISCRIMINATOR,
        BUY_EXACT_IN_DISCRIMINATOR, PUMP_SWAP_BOOST_BUY_EVENT_DISC, PUMP_SWAP_BUY_EVENT_DISC,
        PUMP_SWAP_BUY_IX_NAME_OFFSET, PUMP_SWAP_SELL_EVENT_DISC, PUMP_SWAP_SELL_VQR_OFFSET,
    };
    use yellowstone_grpc_proto::prelude::{
        CompiledInstruction, InnerInstruction, InnerInstructions, Message, Transaction,
        TransactionError, TransactionStatusMeta,
    };

    const PROGRAM_INDEX: u32 = 5;
    /// Every single-pool trade instruction, in the order the fixture emits them.
    const VARIANTS: [&str; 7] = [
        "buy",
        "buy_exact_quote_in",
        "sell",
        "boost_buy_and_burn",
        "buy_v2",
        "buy_exact_quote_in_v2",
        "sell_v2",
    ];
    const N: usize = VARIANTS.len();
    const ROUTER_INDEX: u32 = 6;

    fn instruction(discriminator: [u8; 8], arg0: u64, arg1: u64) -> CompiledInstruction {
        let mut data = discriminator.to_vec();
        data.extend_from_slice(&arg0.to_le_bytes());
        data.extend_from_slice(&arg1.to_le_bytes());
        CompiledInstruction {
            program_id_index: PROGRAM_INDEX,
            accounts: vec![0, 1, 2, 3, 4],
            data,
        }
    }

    /// Reserves every synthetic buy/sell event reports, so market-cap
    /// assertions can be written against known numbers.
    const EVENT_BASE_RESERVES: u64 = 800;
    const EVENT_RAW_QUOTE_RESERVES: u64 = 900;

    /// `virtual_quote` = `Some(v)` builds a post-2026-07-15 payload carrying
    /// `virtual_quote_reserves = v`; `None` builds the pre-upgrade layout,
    /// which is what the pre-existing fixtures exercise.
    fn emitted_event_with_virtual(
        kind: &str,
        token: u64,
        quote: u64,
        stack_height: u32,
        virtual_quote: Option<i128>,
    ) -> InnerInstruction {
        let (disc, len) = match kind {
            "sell" | "sell_v2" => (PUMP_SWAP_SELL_EVENT_DISC, 120),
            "boost_buy_and_burn" => (PUMP_SWAP_BOOST_BUY_EVENT_DISC, 200),
            _ => (PUMP_SWAP_BUY_EVENT_DISC, 120),
        };
        let mut event = vec![0u8; len];
        event[..8].copy_from_slice(&disc);
        if kind == "boost_buy_and_burn" {
            event[152..160].copy_from_slice(&quote.to_le_bytes());
            event[160..168].copy_from_slice(&token.to_le_bytes());
            // virtual_quote_reserves (i128) sits mid-struct here, at 168..184.
            event[168..184].copy_from_slice(&virtual_quote.unwrap_or(0).to_le_bytes());
            event[184..192].copy_from_slice(&EVENT_RAW_QUOTE_RESERVES.to_le_bytes());
            event[192..200].copy_from_slice(&EVENT_BASE_RESERVES.to_le_bytes());
        } else {
            event[16..24].copy_from_slice(&token.to_le_bytes());
            event[48..56].copy_from_slice(&EVENT_BASE_RESERVES.to_le_bytes());
            event[56..64].copy_from_slice(&EVENT_RAW_QUOTE_RESERVES.to_le_bytes());
            let is_sell = kind.starts_with("sell");
            let quote_offset = if is_sell { 64 } else { 112 };
            event[quote_offset..quote_offset + 8].copy_from_slice(&quote.to_le_bytes());

            if let Some(virtual_quote) = virtual_quote {
                // Grow the 120-byte stub out to the full post-upgrade layout.
                // Buy and sell diverge after coin_creator_fee @352 — see
                // `extract_pump_swap_virtual_quote_reserves`.
                if is_sell {
                    event.resize(PUMP_SWAP_SELL_VQR_OFFSET, 0);
                } else {
                    event.resize(PUMP_SWAP_BUY_IX_NAME_OFFSET, 0);
                    let ix_name = b"buy";
                    event.extend_from_slice(&(ix_name.len() as u32).to_le_bytes());
                    event.extend_from_slice(ix_name);
                    // cashback_fee_basis_points, cashback,
                    // buyback_fee_basis_points, buyback_fee
                    event.extend(std::iter::repeat_n(0u8, 8 * 4));
                }
                event.extend_from_slice(&virtual_quote.to_le_bytes());
                event.push(0); // can_boost
                event.extend_from_slice(&0u64.to_le_bytes()); // base_supply
            }
        }

        // Anchor emit_cpi! prefix followed by the event discriminator/payload.
        let mut data = vec![0u8; 8];
        data.extend(event);
        InnerInstruction {
            program_id_index: PROGRAM_INDEX,
            data,
            stack_height: Some(stack_height),
            ..Default::default()
        }
    }

    fn fixture(with_events: bool, failed: bool) -> SubscribeUpdateTransactionInfo {
        fixture_with_virtual(with_events, failed, None)
    }

    fn fixture_with_virtual(
        with_events: bool,
        failed: bool,
        virtual_quote: Option<i128>,
    ) -> SubscribeUpdateTransactionInfo {
        fixture_with_quote(
            with_events,
            failed,
            virtual_quote,
            Pubkey::from_str(WSOL_MINT).unwrap(),
        )
    }

    fn fixture_with_quote(
        with_events: bool,
        failed: bool,
        virtual_quote: Option<i128>,
        quote: Pubkey,
    ) -> SubscribeUpdateTransactionInfo {
        let user = Pubkey::new_unique();
        let global = Pubkey::new_unique();
        let base = Pubkey::new_unique();
        let pump_swap = Pubkey::from_str(PUMP_SWAP_PROGRAM_ID).unwrap();
        let pump = Pubkey::from_str(PUMP_PROGRAM_ID).unwrap();
        let (pool_authority, _) =
            Pubkey::find_program_address(&[b"pool-authority", base.as_ref()], &pump);
        let pool_index = 0u16.to_le_bytes();
        let (pool, _) = Pubkey::find_program_address(
            &[
                b"pool",
                &pool_index,
                pool_authority.as_ref(),
                base.as_ref(),
                quote.as_ref(),
            ],
            &pump_swap,
        );
        let router = Pubkey::new_unique();
        let account_keys = [pool, user, global, base, quote, pump_swap, router]
            .into_iter()
            .map(|key| key.to_bytes().to_vec())
            .collect();

        let variants = [
            ("buy", AMM_BUY_DISCRIMINATOR),
            ("buy_exact_quote_in", BUY_EXACT_IN_DISCRIMINATOR),
            ("sell", AMM_SELL_DISCRIMINATOR),
            ("boost_buy_and_burn", BOOST_BUY_AND_BURN_DISCRIMINATOR),
            ("buy_v2", AMM_BUY_V2_DISCRIMINATOR),
            (
                "buy_exact_quote_in_v2",
                AMM_BUY_EXACT_QUOTE_IN_V2_DISCRIMINATOR,
            ),
            ("sell_v2", AMM_SELL_V2_DISCRIMINATOR),
        ];
        let direct: Vec<_> = variants
            .iter()
            .enumerate()
            .map(|(i, (_, disc))| {
                let arg1 = if failed && i == 0 {
                    u64::MAX
                } else {
                    20 + i as u64
                };
                instruction(*disc, 10 + i as u64, arg1)
            })
            .collect();
        let mut outer = direct.clone();
        outer.push(CompiledInstruction {
            program_id_index: ROUTER_INDEX,
            ..Default::default()
        });

        let mut inner_instructions = Vec::new();
        if with_events {
            for (i, (kind, _)) in variants.iter().enumerate() {
                inner_instructions.push(InnerInstructions {
                    index: i as u32,
                    instructions: vec![emitted_event_with_virtual(
                        kind,
                        100 + i as u64,
                        200 + i as u64,
                        2,
                        virtual_quote,
                    )],
                });
            }

            // The same variants routed through a custom program as CPIs.
            let mut routed = Vec::new();
            for (i, ((kind, _), trade)) in variants.iter().zip(direct).enumerate() {
                routed.push(InnerInstruction {
                    program_id_index: trade.program_id_index,
                    accounts: trade.accounts,
                    data: trade.data,
                    stack_height: Some(2),
                });
                routed.push(emitted_event_with_virtual(
                    kind,
                    100 + i as u64,
                    200 + i as u64,
                    3,
                    virtual_quote,
                ));
            }
            inner_instructions.push(InnerInstructions {
                index: N as u32,
                instructions: routed,
            });
        }

        SubscribeUpdateTransactionInfo {
            signature: vec![7u8; 64],
            transaction: Some(Transaction {
                message: Some(Message {
                    account_keys,
                    instructions: outer,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            meta: Some(TransactionStatusMeta {
                err: failed.then(|| TransactionError { err: vec![1] }),
                inner_instructions,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Drive the real handler and collect one market cap per trade variant.
    async fn market_caps_for(virtual_quote: Option<i128>) -> Vec<Option<u64>> {
        let (sender, mut receiver) = mpsc::channel(16);
        let tx = fixture_with_virtual(true, false, virtual_quote);
        let whitelist = DashSet::new();
        let mint = Pubkey::new_from_array(
            tx.transaction
                .as_ref()
                .unwrap()
                .message
                .as_ref()
                .unwrap()
                .account_keys[3]
                .as_slice()
                .try_into()
                .unwrap(),
        )
        .to_string();
        whitelist.insert(mint);
        process_pump_swap_tx(tx, 42, &whitelist, &sender, false)
            .await
            .unwrap();
        drop(sender);
        let mut caps = Vec::new();
        for _ in 0..2 * N {
            caps.push(receiver.recv().await.unwrap().market_cap);
        }
        caps
    }

    /// End-to-end wiring check: a non-zero `virtual_quote_reserves` must reach
    /// the market cap through the real handler, for buy, buy_exact_quote_in and
    /// sell alike — each of which decodes a different event layout.
    ///
    /// Doubling the quote side (raw 900 + virtual 900) doubles the price, and
    /// hence the market cap, since the base side is untouched.
    #[tokio::test]
    async fn virtual_quote_reserves_reach_market_cap_for_every_variant() {
        let baseline = market_caps_for(None).await;
        let boosted = market_caps_for(Some(EVENT_RAW_QUOTE_RESERVES as i128)).await;

        for (i, (base, boost)) in baseline.iter().zip(&boosted).enumerate() {
            let base = base.expect("baseline market cap");
            let boost = boost.expect("boosted market cap");
            assert_eq!(
                boost,
                base * 2,
                "variant {} (index {i}) did not price on effective reserves",
                VARIANTS[i % N]
            );
        }
    }

    /// A negative adjustment that cancels the vault balance leaves no quote
    /// side, so there is no meaningful price to report.
    #[tokio::test]
    async fn fully_offset_virtual_reserves_zero_the_market_cap() {
        for cap in market_caps_for(Some(-(EVENT_RAW_QUOTE_RESERVES as i128))).await {
            assert_eq!(cap, Some(0));
        }
    }

    /// Phase 1 shipped the field as 0 on every pool; quotes must be unchanged
    /// from the pre-upgrade payloads the other tests exercise.
    #[tokio::test]
    async fn phase_one_zero_virtual_reserves_change_nothing() {
        assert_eq!(market_caps_for(Some(0)).await, market_caps_for(None).await);
    }

    #[tokio::test]
    async fn captures_every_current_trade_variant_direct_and_through_cpi() {
        let (sender, mut receiver) = mpsc::channel(16);
        let tx = fixture(true, false);
        let whitelist = DashSet::new();
        let mint = Pubkey::new_from_array(
            tx.transaction
                .as_ref()
                .unwrap()
                .message
                .as_ref()
                .unwrap()
                .account_keys[3]
                .as_slice()
                .try_into()
                .unwrap(),
        )
        .to_string();
        whitelist.insert(mint.clone());
        process_pump_swap_tx(tx, 42, &whitelist, &sender, false)
            .await
            .unwrap();
        drop(sender);

        let mut records = Vec::new();
        for _ in 0..2 * N {
            records.push(receiver.recv().await.unwrap());
        }
        assert_eq!(
            records
                .iter()
                .map(|record| record.instruction_type.as_str())
                .collect::<Vec<_>>(),
            [VARIANTS, VARIANTS].concat()
        );
        for (i, record) in records.iter().enumerate() {
            let variant = i % N;
            assert!(record.success);
            assert_eq!(record.amount_source, "event");
            assert_eq!(record.token_amount, 100 + variant as u64);
            assert_eq!(record.sol_amount, 200 + variant as u64);
            assert_eq!(record.ix_index, i as i32);
            assert_eq!(record.is_cpi, i >= N);
            assert_eq!(record.slot, 42);
            assert_eq!(record.mint_address, mint);
            assert!(!record.is_usdc);
            assert!(record.market_cap.is_some());
        }
    }

    #[tokio::test]
    async fn retains_failed_eventless_trade_attempts_from_instruction_bounds() {
        let (sender, mut receiver) = mpsc::channel(8);
        let tx = fixture(false, true);
        let whitelist = DashSet::new();
        let mint = Pubkey::new_from_array(
            tx.transaction
                .as_ref()
                .unwrap()
                .message
                .as_ref()
                .unwrap()
                .account_keys[3]
                .as_slice()
                .try_into()
                .unwrap(),
        )
        .to_string();
        whitelist.insert(mint);
        process_pump_swap_tx(tx, 43, &whitelist, &sender, false)
            .await
            .unwrap();
        drop(sender);

        for i in 0..N as u64 {
            let record = receiver.recv().await.unwrap();
            assert_eq!(record.instruction_type, VARIANTS[i as usize]);
            assert!(!record.success);
            assert_eq!(record.amount_source, "instruction_request");
            let exact_in = matches!(i, 1 | 3 | 5);
            assert_eq!(record.token_amount, if exact_in { 20 + i } else { 10 + i });
            assert_eq!(
                record.sol_amount,
                if i == 0 {
                    u64::MAX
                } else if exact_in {
                    10 + i
                } else {
                    20 + i
                }
            );
        }
    }

    #[tokio::test]
    async fn preserves_intentional_whitelist_and_pool_safety_filters() {
        let (sender, mut receiver) = mpsc::channel(32);

        // Canonical but not whitelisted.
        let tx = fixture(true, false);
        process_pump_swap_tx(tx, 44, &DashSet::new(), &sender, false)
            .await
            .unwrap();
        assert!(receiver.try_recv().is_err());

        // WSOL-base pools stay excluded even if explicitly whitelisted.
        let mut tx = fixture(true, false);
        tx.transaction
            .as_mut()
            .unwrap()
            .message
            .as_mut()
            .unwrap()
            .account_keys[3] = Pubkey::from_str(WSOL_MINT).unwrap().to_bytes().to_vec();
        let whitelist = DashSet::new();
        whitelist.insert(WSOL_MINT.to_string());
        process_pump_swap_tx(tx, 45, &whitelist, &sender, false)
            .await
            .unwrap();
        assert!(receiver.try_recv().is_err());

        // A pool address that is not the canonical PDA of its pair (swapped
        // quote account or a foreign pool) stays excluded.
        for account_index in [4usize, 0usize] {
            let mut tx = fixture(true, false);
            let message = tx.transaction.as_mut().unwrap().message.as_mut().unwrap();
            let mint = Pubkey::new_from_array(message.account_keys[3].clone().try_into().unwrap())
                .to_string();
            message.account_keys[account_index] = Pubkey::new_unique().to_bytes().to_vec();
            let whitelist = DashSet::new();
            whitelist.insert(mint);
            process_pump_swap_tx(tx, 46, &whitelist, &sender, false)
                .await
                .unwrap();
            assert!(receiver.try_recv().is_err());
        }
    }

    /// Canonical pools quoted in anything other than SOL/USDC (stock tokens,
    /// pump coins) are recorded in the quote's own atomic units, identified
    /// by `quote_mint_address`, success and failure alike.
    #[tokio::test]
    async fn records_canonical_pools_with_any_quote_mint() {
        let quote = Pubkey::new_unique();
        for failed in [false, true] {
            let tx = fixture_with_quote(!failed, failed, None, quote);
            let message = tx.transaction.as_ref().unwrap().message.as_ref().unwrap();
            let mint = Pubkey::new_from_array(message.account_keys[3].clone().try_into().unwrap());
            let records = records_of(tx, &mint).await;
            assert_eq!(records.len(), if failed { N } else { 2 * N });
            for r in &records {
                assert_eq!(r.quote_mint_address, quote.to_string());
                assert_eq!(r.mint_address, mint.to_string());
                assert!(!r.is_usdc);
                assert_eq!(r.success, !failed);
                if failed {
                    assert_eq!(r.market_cap, None);
                } else {
                    assert!(r.market_cap.is_some_and(|mc| mc > 0));
                }
            }
        }
    }

    // === multi_hop_swap (synthetic) ===

    /// A one-hop `multi_hop_swap` on a canonical SOL pool. Accounts: 0 user,
    /// 1 user_in_token_account, 2..16 other fixed accounts, 16..21 the hop
    /// group, 21 PumpSwap. `event_pool` is the pool the BuyEvent / SellEvent
    /// reports; `None` means a failed tx (no event).
    fn single_hop_route(
        is_buy: bool,
        event_pool: Option<Option<Pubkey>>,
    ) -> (SubscribeUpdateTransactionInfo, Pubkey, Pubkey, Pubkey) {
        let base = Pubkey::new_unique();
        let quote = Pubkey::from_str(WSOL_MINT).unwrap();
        let pump_swap = Pubkey::from_str(PUMP_SWAP_PROGRAM_ID).unwrap();
        let pump = Pubkey::from_str(PUMP_PROGRAM_ID).unwrap();
        let pool = canonical_pump_swap_pool(&base, &quote, &pump, &pump_swap);
        let user = Pubkey::new_unique();
        let mut keys: Vec<Pubkey> = vec![user];
        keys.extend((1..16).map(|_| Pubkey::new_unique()));
        keys.extend([
            base,
            quote,
            pool,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        ]);
        keys.push(pump_swap);
        let program_index = (keys.len() - 1) as u32;

        let mut data = AMM_MULTI_HOP_SWAP_DISCRIMINATOR.to_vec();
        data.extend_from_slice(&1_000u64.to_le_bytes()); // amount_in
        data.extend_from_slice(&900u64.to_le_bytes()); // min_amount_out
        let ix = CompiledInstruction {
            program_id_index: program_index,
            accounts: (0..21).collect(),
            data,
        };

        let mut inner_instructions = Vec::new();
        if let Some(event_pool) = event_pool {
            let mut event = vec![0u8; 200];
            let disc = if is_buy {
                PUMP_SWAP_BUY_EVENT_DISC
            } else {
                PUMP_SWAP_SELL_EVENT_DISC
            };
            event[..8].copy_from_slice(&disc);
            event[16..24].copy_from_slice(&5_000u64.to_le_bytes());
            event[48..56].copy_from_slice(&EVENT_BASE_RESERVES.to_le_bytes());
            event[56..64].copy_from_slice(&EVENT_RAW_QUOTE_RESERVES.to_le_bytes());
            let quote_offset = if is_buy { 112 } else { 64 };
            event[quote_offset..quote_offset + 8].copy_from_slice(&700u64.to_le_bytes());
            event[120..152].copy_from_slice(event_pool.unwrap_or(pool).as_ref());
            let mut data = vec![0u8; 8];
            data.extend(event);
            inner_instructions.push(InnerInstructions {
                index: 0,
                instructions: vec![InnerInstruction {
                    program_id_index: program_index,
                    data,
                    stack_height: Some(2),
                    ..Default::default()
                }],
            });
        }
        // The account the route spends from holds the quote on a buy route
        // and the base on a sell route — the failed path's only side signal.
        let spent = if is_buy { quote } else { base };
        let tx = SubscribeUpdateTransactionInfo {
            signature: vec![9u8; 64],
            transaction: Some(Transaction {
                message: Some(Message {
                    account_keys: keys.iter().map(|k| k.to_bytes().to_vec()).collect(),
                    instructions: vec![ix],
                    ..Default::default()
                }),
                ..Default::default()
            }),
            meta: Some(TransactionStatusMeta {
                err: event_pool
                    .is_none()
                    .then(|| TransactionError { err: vec![1] }),
                inner_instructions,
                pre_token_balances: vec![yellowstone_grpc_proto::prelude::TokenBalance {
                    account_index: 1,
                    mint: spent.to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        (tx, user, base, pool)
    }

    async fn records_of(tx: SubscribeUpdateTransactionInfo, mint: &Pubkey) -> Vec<TradeRecord> {
        let (sender, mut receiver) = mpsc::channel(64);
        let whitelist = DashSet::new();
        whitelist.insert(mint.to_string());
        process_pump_swap_tx(tx, 50, &whitelist, &sender, false)
            .await
            .unwrap();
        drop(sender);
        let mut out = Vec::new();
        while let Some(r) = receiver.recv().await {
            out.push(r);
        }
        out
    }

    /// A one-hop route trades the user's own coin, so it is attributed to the
    /// user (`multi_hop_swap`, never `_routed`), with the event's amounts.
    #[tokio::test]
    async fn single_hop_route_is_the_users_own_trade() {
        for is_buy in [true, false] {
            let (tx, user, base, pool) = single_hop_route(is_buy, Some(None));
            let records = records_of(tx, &base).await;
            assert_eq!(records.len(), 1);
            let r = &records[0];
            assert_eq!(r.instruction_type, MULTI_HOP_INSTRUCTION_TYPE);
            assert_eq!(r.user_pubkey, user.to_string());
            assert_eq!(r.mint_address, base.to_string());
            assert_eq!(r.pool_address, pool.to_string());
            assert_eq!(r.quote_mint_address, WSOL_MINT);
            assert_eq!(r.is_buy, is_buy);
            assert_eq!((r.token_amount, r.sol_amount), (5_000, 700));
            assert_eq!(r.amount_source, "event");
            assert!(r.market_cap.is_some());
            assert!(!r.is_cpi);
        }
    }

    /// Failed one-hop route: the side comes from the mint of the spent
    /// account; `amount_in` sits on the input side and `min_amount_out` on
    /// the output side.
    #[tokio::test]
    async fn failed_single_hop_route_reads_side_from_the_spent_account() {
        for is_buy in [true, false] {
            let (tx, _, base, _) = single_hop_route(is_buy, None);
            let records = records_of(tx, &base).await;
            assert_eq!(records.len(), 1);
            let r = &records[0];
            assert!(!r.success);
            assert_eq!(r.is_buy, is_buy);
            assert_eq!(r.amount_source, "instruction_request");
            assert_eq!(r.market_cap, None);
            let expected = if is_buy { (900, 1_000) } else { (1_000, 900) };
            assert_eq!((r.token_amount, r.sol_amount), expected);
        }
    }

    /// An event whose pool is not one of the route's hops is never
    /// attributed to a hop.
    #[tokio::test]
    async fn multi_hop_event_for_an_unknown_pool_is_ignored() {
        let (tx, _, base, _) = single_hop_route(true, Some(Some(Pubkey::new_unique())));
        assert!(records_of(tx, &base).await.is_empty());
    }
}
