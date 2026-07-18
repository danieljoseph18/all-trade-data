use anyhow::Result;
use chrono::Utc;
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

use crate::database::TradeRecord;
use crate::utils::{
    AMM_BUY_DISCRIMINATOR, AMM_SELL_DISCRIMINATOR, BOOST_BUY_AND_BURN_DISCRIMINATOR,
    BUY_EXACT_IN_DISCRIMINATOR, PUMP_PROGRAM_ID, PUMP_SWAP_BOOST_BUY_EVENT_DISC,
    PUMP_SWAP_BUY_EVENT_DISC, PUMP_SWAP_PROGRAM_ID, PUMP_SWAP_SELL_EVENT_DISC, WSOL_MINT,
    effective_quote_reserves, extract_boost_buy_event, extract_pool_reserves_from_data,
    extract_pump_swap_requested_amounts, extract_pump_swap_virtual_quote_reserves,
    extract_quote_volume, extract_transaction_amounts, extract_transaction_fees, find_event_data,
    get_market_cap_from_reserves, get_program_instructions, is_canonical_pump_swap_pool,
    quote_currency_of, resolve_instruction_account, resolve_pump_swap_memecoin,
    resolve_pump_swap_pool, resolve_pump_swap_quote_mint,
};

/// Per-process running count of trades emitted (across all txs). Used by readonly
/// validation logs so the operator can quickly see throughput.
static TRADE_COUNTER: AtomicU64 = AtomicU64::new(0);

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

/// Parse a Pump Swap transaction, extract every trade instruction (top-level + CPI),
/// and send a record per trade to the channel.
///
/// Detection is discriminator-based on the instruction `data` (canonical Anchor
/// approach) — independent of log truncation and stable for failed txs. Event
/// payloads are read from inner-instruction `emit_cpi!` data (not "Program data:"
/// logs) so multi-trade transactions are attributed correctly via stack_height
/// bounds in `find_event_data`.
async fn process_pump_swap_tx(
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

    let tx_signature = bs58::encode(&tx_data.signature).into_string();
    let now = Utc::now();

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

    let mut trade_index = 0i32;
    for (instr, parent_outer_idx, start_inner_pos) in &all_instrs {
        if instr.data.len() < 8 {
            continue;
        }
        let disc = &instr.data[..8];

        let (instruction_type, is_buy, is_exact_in, event_disc) =
            if disc == AMM_BUY_DISCRIMINATOR.as_slice() {
                ("buy", true, false, &PUMP_SWAP_BUY_EVENT_DISC)
            } else if disc == BUY_EXACT_IN_DISCRIMINATOR.as_slice() {
                ("buy_exact_quote_in", true, true, &PUMP_SWAP_BUY_EVENT_DISC)
            } else if disc == AMM_SELL_DISCRIMINATOR.as_slice() {
                ("sell", false, false, &PUMP_SWAP_SELL_EVENT_DISC)
            } else if disc == BOOST_BUY_AND_BURN_DISCRIMINATOR.as_slice() {
                (
                    "boost_buy_and_burn",
                    true,
                    true,
                    &PUMP_SWAP_BOOST_BUY_EVENT_DISC,
                )
            } else {
                continue;
            };

        let Some(mint) = resolve_pump_swap_memecoin(instr, &full_accounts) else {
            continue;
        };
        if mint == WSOL_MINT {
            continue;
        }
        let Some(quote_mint) = resolve_pump_swap_quote_mint(instr, &full_accounts) else {
            continue;
        };
        let quote_currency = quote_currency_of(&quote_mint);
        if quote_currency.is_none() {
            continue;
        }
        if !readonly && !whitelist.contains(&mint) {
            continue;
        }
        let canonical =
            is_canonical_pump_swap_pool(instr, &full_accounts, &pump_program_id, &program_id);
        if !canonical {
            continue;
        }
        let Some(pool_address) = resolve_pump_swap_pool(instr, &full_accounts) else {
            continue;
        };
        let user = resolve_instruction_account(instr, &full_accounts, 1)
            .unwrap_or_else(|| "unknown".to_string());
        let is_usdc = quote_currency.is_some_and(|currency| currency.is_usdc());
        let requested_amounts = || {
            let (token_amount, quote_volume) =
                extract_pump_swap_requested_amounts(&instr.data, is_exact_in);
            (
                token_amount,
                quote_volume,
                None,
                0,
                0,
                "instruction_request",
            )
        };

        // Successful txs: pull executed amounts/reserves from the BuyEvent/
        // SellEvent self-CPI and compute market cap. Failed txs emit no event,
        // so record the *requested* amounts from the instruction args and leave
        // market cap NULL.
        let (token_amount, quote_volume, market_cap, base_reserves, quote_reserves, amount_source) =
            if tx_succeeded {
                if let Some(event_data) = find_event_data(
                    meta,
                    *parent_outer_idx,
                    *start_inner_pos,
                    instr.program_id_index,
                    event_disc,
                ) {
                    if instruction_type == "boost_buy_and_burn" {
                        if let Some((token_amount, quote_volume, base_reserves, quote_reserves)) =
                            extract_boost_buy_event(event_data)
                        {
                            let market_cap = if canonical && mint != WSOL_MINT {
                                quote_currency.map(|currency| {
                                    get_market_cap_from_reserves(
                                        base_reserves,
                                        quote_reserves,
                                        currency,
                                    )
                                })
                            } else {
                                None
                            };
                            (
                                token_amount,
                                quote_volume,
                                market_cap,
                                base_reserves,
                                quote_reserves,
                                "event",
                            )
                        } else {
                            requested_amounts()
                        }
                    } else {
                        let (base_reserves, quote_reserves) =
                            extract_pool_reserves_from_data(event_data);
                        let (buy_vol, sell_vol) = extract_quote_volume(event_data);
                        let quote_volume = if is_buy { buy_vol } else { sell_vol };
                        if let (
                            Some(base_reserves),
                            Some(raw_quote_reserves),
                            Some(quote_volume),
                            Some(token_amount),
                        ) = (
                            base_reserves,
                            quote_reserves,
                            quote_volume,
                            extract_transaction_amounts(event_data),
                        ) {
                            // Price against effective quote reserves, not the
                            // raw vault balance — matches what the boost path
                            // above already reports. Absent on pre-2026-07-15
                            // payloads, where effective == raw.
                            let quote_reserves = effective_quote_reserves(
                                raw_quote_reserves,
                                extract_pump_swap_virtual_quote_reserves(event_data, is_buy),
                            );
                            let market_cap = if canonical && mint != WSOL_MINT {
                                // BuyEvent/SellEvent pool reserves are the
                                // post-trade values. Applying the trade delta
                                // again creates a phantom price one fill ahead.
                                quote_currency.map(|currency| {
                                    get_market_cap_from_reserves(
                                        base_reserves,
                                        quote_reserves,
                                        currency,
                                    )
                                })
                            } else {
                                None
                            };
                            (
                                token_amount,
                                quote_volume,
                                market_cap,
                                base_reserves,
                                quote_reserves,
                                "event",
                            )
                        } else {
                            requested_amounts()
                        }
                    }
                } else {
                    requested_amounts()
                }
            } else {
                requested_amounts()
            };

        let record = TradeRecord {
            tx_signature: tx_signature.clone(),
            ix_index: trade_index,
            pool_address,
            mint_address: mint.clone(),
            quote_mint_address: quote_mint.clone(),
            instruction_type: instruction_type.to_string(),
            amount_source: amount_source.to_string(),
            is_cpi: *start_inner_pos > 0,
            user_pubkey: user.clone(),
            is_buy,
            token_amount,
            sol_amount: quote_volume,
            market_cap,
            is_usdc,
            success: tx_succeeded,
            slot: slot as i64,
            created_at: now,
            priority_fee: priority_fee.map(|v| v as i64),
            transfer_tip: transfer_tip.map(|v| v as i64),
            tip_provider: tip_provider.clone(),
            compute_units_consumed,
            lamports_per_compute_unit,
        };

        if readonly {
            // Validation log — every field that ends up in the DB, plus the
            // human-readable SOL conversions, so a tx can be looked up via RPC
            // and cross-checked.
            let n = TRADE_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
            let quote_base_unit = quote_currency.map_or(1.0, |currency| currency.base_unit());
            let quote_amount_f = quote_volume as f64 / quote_base_unit;
            let token_amount_f = token_amount as f64 / 1_000_000.0;
            let market_cap_quote = market_cap.map(|m| m as f64 / quote_base_unit);
            info!(
                "[TRADE #{n}] slot={slot} sig={sig} ix={ix} {side} mint={mint} user={user} \
                 route={route} success={succ} quote={quote:.6} quote_ccy={ccy} is_usdc={is_usdc} tok={tok:.3} \
                 mc={mc:?} base_res={br} quote_res={qr} prio={prio:?} \
                 tip={tip:?} provider={prov:?} cu={cu:?} lpcu={lpcu:?}",
                n = n,
                slot = slot,
                sig = tx_signature,
                ix = trade_index,
                side = if is_buy { "BUY " } else { "SELL" },
                mint = mint,
                user = user,
                route = if *start_inner_pos > 0 {
                    "cpi"
                } else {
                    "direct"
                },
                succ = tx_succeeded,
                quote = quote_amount_f,
                ccy = quote_currency.map_or("OTHER", |currency| currency.label()),
                is_usdc = is_usdc,
                tok = token_amount_f,
                mc = market_cap_quote,
                br = base_reserves,
                qr = quote_reserves,
                prio = priority_fee,
                tip = transfer_tip,
                prov = tip_provider,
                cu = compute_units_consumed,
                lpcu = lamports_per_compute_unit,
            );
        } else if let Err(e) = sender.send(record).await {
            warn!("Trade channel closed; unable to persist trade: {:?}", e);
        }
        trade_index += 1;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::{
        AMM_BUY_DISCRIMINATOR, AMM_SELL_DISCRIMINATOR, BOOST_BUY_AND_BURN_DISCRIMINATOR,
        BUY_EXACT_IN_DISCRIMINATOR, PUMP_SWAP_BOOST_BUY_EVENT_DISC, PUMP_SWAP_BUY_EVENT_DISC,
        PUMP_SWAP_BUY_IX_NAME_OFFSET, PUMP_SWAP_SELL_EVENT_DISC, PUMP_SWAP_SELL_VQR_OFFSET,
    };
    use yellowstone_grpc_proto::prelude::{
        CompiledInstruction, InnerInstruction, InnerInstructions, Message, Transaction,
        TransactionError, TransactionStatusMeta,
    };

    const PROGRAM_INDEX: u32 = 5;
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
            "sell" => (PUMP_SWAP_SELL_EVENT_DISC, 120),
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
            let quote_offset = if kind == "sell" { 64 } else { 112 };
            event[quote_offset..quote_offset + 8].copy_from_slice(&quote.to_le_bytes());

            if let Some(virtual_quote) = virtual_quote {
                // Grow the 120-byte stub out to the full post-upgrade layout.
                // Buy and sell diverge after coin_creator_fee @352 — see
                // `extract_pump_swap_virtual_quote_reserves`.
                if kind == "sell" {
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
        let user = Pubkey::new_unique();
        let global = Pubkey::new_unique();
        let base = Pubkey::new_unique();
        let quote = Pubkey::from_str(WSOL_MINT).unwrap();
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

            // The same four variants routed through a custom program as CPIs.
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
                index: 4,
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
        let mut caps = Vec::new();
        for _ in 0..8 {
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
                ["buy", "buy_exact_quote_in", "sell", "boost_buy_and_burn"][i % 4]
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

        let mut records = Vec::new();
        for _ in 0..8 {
            records.push(receiver.recv().await.unwrap());
        }
        assert_eq!(
            records
                .iter()
                .map(|record| record.instruction_type.as_str())
                .collect::<Vec<_>>(),
            [
                "buy",
                "buy_exact_quote_in",
                "sell",
                "boost_buy_and_burn",
                "buy",
                "buy_exact_quote_in",
                "sell",
                "boost_buy_and_burn",
            ]
        );
        for (i, record) in records.iter().enumerate() {
            let variant = i % 4;
            assert!(record.success);
            assert_eq!(record.amount_source, "event");
            assert_eq!(record.token_amount, 100 + variant as u64);
            assert_eq!(record.sol_amount, 200 + variant as u64);
            assert_eq!(record.ix_index, i as i32);
            assert_eq!(record.is_cpi, i >= 4);
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

        for i in 0..4 {
            let record = receiver.recv().await.unwrap();
            assert!(!record.success);
            assert_eq!(record.amount_source, "instruction_request");
            let exact_in = matches!(i, 1 | 3);
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

        // Arbitrary quote mints and noncanonical pool addresses stay excluded.
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
}
