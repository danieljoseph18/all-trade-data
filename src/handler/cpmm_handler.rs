//! Raydium CPMM handler: records every post-migration stonk.fun swap on a
//! tracked `alt_mints` row into `alt_trades`.
//!
//! Mirrors `amm_handler.rs` for PumpSwap: discriminator-based instruction
//! detection (top-level + CPI), failed transactions retained with the
//! requested amounts, tx-level fees replicated to every trade in the tx.
//!
//! Pool safety: a swap is accepted only when its `amm_config` is the Stonk
//! migration config **and** its `pool_state` is either the derived PDA for
//! `(config, base, launch quote)` or the pool strategy-data recorded from an
//! observed `migrate_to_cpswap`. Anyone can open extra CPMM pools for a
//! tracked mint under another config or quote; those are never the
//! migration pool and are dropped here.

use anyhow::Result;
use chrono::Utc;
use log::{info, warn};
use solana_program::pubkey::Pubkey;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;
use yellowstone_grpc_proto::geyser::SubscribeUpdateTransactionInfo;

use crate::database::{AltMintInfo, AltMintMap, AltTradeRecord};
use crate::utils::{
    CPMM_SWAP_AMM_CONFIG_IX_POS, CPMM_SWAP_BASE_INPUT_DISCRIMINATOR,
    CPMM_SWAP_BASE_OUTPUT_DISCRIMINATOR, CPMM_SWAP_EVENT_DISC, CPMM_SWAP_INPUT_MINT_IX_POS,
    CPMM_SWAP_INPUT_VAULT_IX_POS, CPMM_SWAP_MIN_ACCOUNTS, CPMM_SWAP_OUTPUT_MINT_IX_POS,
    CPMM_SWAP_OUTPUT_VAULT_IX_POS, CPMM_SWAP_PAYER_IX_POS, CPMM_SWAP_POOL_STATE_IX_POS,
    CpmmSwapEvent, RAYDIUM_CPMM_PROGRAM_ID, RAYDIUM_CPMM_VENUE_NAME, STONK_CPMM_AMM_CONFIG,
    STONK_PLATFORM_NAME, USDC_MINT, WSOL_MINT, cpmm_canonical_pool, cpmm_market_cap,
    cpmm_requested_amounts, cpmm_resolve_side, cpmm_swap_events_from_logs,
    cpmm_swap_from_vault_balances, extract_transaction_fees, find_event_data,
    get_program_instructions, instruction_account_pubkey, resolve_instruction_account,
    token_decimals_from_meta,
};

static ALT_TRADE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates the gRPC handler closure that parses Raydium CPMM transactions
/// and sends every qualifying stonk.fun trade record to the alt batch
/// inserter. `readonly`: log instead of send.
pub fn create_cpmm_handler(
    alt_mints: AltMintMap,
    sender: mpsc::Sender<AltTradeRecord>,
    readonly: bool,
) -> impl Fn(SubscribeUpdateTransactionInfo, u64) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>
+ Clone
+ Send
+ Sync
+ 'static {
    move |tx_data: SubscribeUpdateTransactionInfo, slot: u64| {
        let alt_mints = alt_mints.clone();
        let sender = sender.clone();
        Box::pin(async move { process_cpmm_tx(&tx_data, slot, &alt_mints, &sender, readonly).await })
    }
}

/// Parse a Raydium CPMM transaction and send a record per stonk.fun swap.
pub async fn process_cpmm_tx(
    tx_data: &SubscribeUpdateTransactionInfo,
    slot: u64,
    alt_mints: &dashmap::DashMap<String, AltMintInfo>,
    sender: &mpsc::Sender<AltTradeRecord>,
    readonly: bool,
) -> Result<()> {
    let tx_succeeded = tx_data.meta.as_ref().is_none_or(|m| m.err.is_none());

    let program_id = Pubkey::from_str(RAYDIUM_CPMM_PROGRAM_ID)?;
    let stonk_config = Pubkey::from_str(STONK_CPMM_AMM_CONFIG)?;

    let msg = tx_data
        .transaction
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Transaction missing"))?
        .message
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Message missing"))?;
    let meta = tx_data
        .meta
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Meta missing"))?;

    let mut full_accounts = msg.account_keys.clone();
    full_accounts.extend(meta.loaded_writable_addresses.iter().cloned());
    full_accounts.extend(meta.loaded_readonly_addresses.iter().cloned());

    // `get_program_instructions` lists top-level calls first, then CPIs.
    // `SwapEvent`s come from the log stream in execution order, so sort the
    // instructions the same way before matching events by pool.
    let mut all_instrs = get_program_instructions(msg, meta, &full_accounts, &program_id);
    if all_instrs.is_empty() {
        return Ok(());
    }
    all_instrs.sort_by_key(|(_, outer, inner)| (*outer, *inner));

    let tx_signature = bs58::encode(&tx_data.signature).into_string();
    let now = Utc::now();
    let (priority_fee, transfer_tip, tip_provider) =
        extract_transaction_fees(msg, meta, &full_accounts);
    let compute_units_consumed = meta.compute_units_consumed.map(|v| v as i64);
    let lamports_per_compute_unit = priority_fee.map(|p| p as f64 / 1_000_000.0);

    // SwapEvents from the log stream, decoded on first use: ~99% of CPMM
    // traffic is not a Stonk pool and never gets this far. Failed txs emit
    // no events and never decode.
    let mut log_events: Option<Vec<CpmmSwapEvent>> = None;

    let mut trade_index = 0i32;
    for (instr, parent_outer_idx, start_inner_pos) in &all_instrs {
        if instr.data.len() < 8 {
            continue;
        }
        let disc = &instr.data[..8];
        let instruction_type = if disc == CPMM_SWAP_BASE_INPUT_DISCRIMINATOR.as_slice() {
            "swap_base_input"
        } else if disc == CPMM_SWAP_BASE_OUTPUT_DISCRIMINATOR.as_slice() {
            "swap_base_output"
        } else {
            continue;
        };
        if instr.accounts.len() < CPMM_SWAP_MIN_ACCOUNTS {
            continue;
        }

        let resolve = |pos: usize| resolve_instruction_account(instr, &full_accounts, pos);
        let resolve_key = |pos: usize| instruction_account_pubkey(instr, &full_accounts, pos);

        // Every Stonk migration pool lives under the platform's cpswap_config.
        let Some(amm_config) = resolve_key(CPMM_SWAP_AMM_CONFIG_IX_POS) else {
            continue;
        };
        if amm_config != stonk_config {
            continue;
        }
        let (Some(pool_key), Some(input_key), Some(output_key)) = (
            resolve_key(CPMM_SWAP_POOL_STATE_IX_POS),
            resolve_key(CPMM_SWAP_INPUT_MINT_IX_POS),
            resolve_key(CPMM_SWAP_OUTPUT_MINT_IX_POS),
        ) else {
            continue;
        };
        let input_mint = input_key.to_string();
        let output_mint = output_key.to_string();

        // Which leg is a tracked stonk.fun mint. A tracked-vs-tracked pair
        // (two Stonk mints) cannot be a migration pool for either, since the
        // launch quote is never a Stonk base mint. Each lookup clones and
        // releases its shard lock before the next, so a concurrent refresh
        // can never wedge between two held read guards.
        let input_info = alt_mints.get(&input_mint).map(|r| r.value().clone());
        let output_info = alt_mints.get(&output_mint).map(|r| r.value().clone());
        let (mint, quote_mint, base_is_input, info) = match (input_info, output_info) {
            (Some(info), None) => (input_mint.clone(), output_mint.clone(), true, info),
            (None, Some(info)) => (output_mint.clone(), input_mint.clone(), false, info),
            _ => continue,
        };
        if quote_mint != info.quote_mint {
            continue;
        }

        let pool_address = pool_key.to_string();
        let expected_pool =
            cpmm_canonical_pool(&program_id, &stonk_config, &input_key, &output_key);
        let recorded_pool = info.cpmm_pool.as_deref() == Some(pool_address.as_str());
        if pool_key != expected_pool && !recorded_pool {
            continue;
        }

        let user = resolve(CPMM_SWAP_PAYER_IX_POS).unwrap_or_else(|| "unknown".to_string());
        let quote_decimals = info
            .quote_decimals
            .or_else(|| token_decimals_from_meta(meta, &quote_mint).map(i16::from));

        // Successful txs: the SwapEvent log matched by pool id in execution
        // order; emit_cpi as a second path in case Raydium changes emission
        // style; vault balance deltas as the last resort for truncated logs.
        let event = if tx_succeeded {
            let events = log_events
                .get_or_insert_with(|| cpmm_swap_events_from_logs(&meta.log_messages));
            match events.iter().position(|e| e.pool_id == pool_address) {
                Some(i) => Some((events.remove(i), "event")),
                None => find_event_data(
                    meta,
                    *parent_outer_idx,
                    *start_inner_pos,
                    instr.program_id_index,
                    &CPMM_SWAP_EVENT_DISC,
                )
                .and_then(CpmmSwapEvent::parse)
                .map(|e| (e, "event")),
            }
            .or_else(|| {
                let vault_index = |pos: usize| {
                    instr
                        .accounts
                        .get(pos)
                        .map(|i| *i as usize)
                        .filter(|i| *i < full_accounts.len())
                };
                let (Some(in_vault), Some(out_vault)) = (
                    vault_index(CPMM_SWAP_INPUT_VAULT_IX_POS),
                    vault_index(CPMM_SWAP_OUTPUT_VAULT_IX_POS),
                ) else {
                    return None;
                };
                let fallback = cpmm_swap_from_vault_balances(
                    meta,
                    &pool_address,
                    in_vault,
                    out_vault,
                    &input_mint,
                    &output_mint,
                );
                if fallback.is_some() {
                    info!(
                        "CPMM swap on {} without a SwapEvent log — reconstructed from vault balances (tx {}, slot {}, {} log lines)",
                        pool_address,
                        tx_signature,
                        slot,
                        meta.log_messages.len()
                    );
                }
                fallback.map(|e| (e, "vault_balances"))
            })
        } else {
            None
        };

        let side = event
            .as_ref()
            .and_then(|(e, _)| cpmm_resolve_side(e, &mint).map(|s| (s, e)));

        let (
            is_buy,
            token_amount,
            quote_amount,
            market_cap,
            base_reserves,
            quote_reserves,
            base_transfer_fee,
            trade_fee,
            creator_fee,
            amount_source,
        ) = match (&side, &event) {
            (Some((s, e)), Some((_, source))) => (
                s.is_buy,
                s.base_amount,
                s.quote_amount,
                Some(cpmm_market_cap(
                    s.quote_reserve_after,
                    s.base_reserve_after,
                    info.total_supply,
                )),
                Some(s.base_reserve_after),
                Some(s.quote_reserve_after),
                Some(s.base_transfer_fee),
                Some(e.trade_fee),
                Some(e.creator_fee),
                *source,
            ),
            _ => {
                // Failed tx, or a successful one whose event could not be
                // recovered: record the requested bounds from the ix args.
                let (token_amount, quote_amount) =
                    cpmm_requested_amounts(&instr.data, base_is_input);
                (
                    !base_is_input,
                    token_amount,
                    quote_amount,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    "instruction_request",
                )
            }
        };
        if tx_succeeded && amount_source == "instruction_request" {
            warn!(
                "CPMM swap on {} (tx {}) succeeded but no SwapEvent was recoverable; recorded requested amounts",
                pool_address, tx_signature
            );
        }

        let record = AltTradeRecord {
            tx_signature: tx_signature.clone(),
            ix_index: trade_index,
            platform: STONK_PLATFORM_NAME.to_string(),
            venue: RAYDIUM_CPMM_VENUE_NAME.to_string(),
            pool_address: pool_address.clone(),
            mint_address: mint.clone(),
            quote_mint_address: quote_mint.clone(),
            instruction_type: instruction_type.to_string(),
            amount_source: amount_source.to_string(),
            is_cpi: *start_inner_pos > 0,
            user_pubkey: user.clone(),
            is_buy,
            token_amount,
            quote_amount,
            market_cap,
            base_reserves,
            quote_reserves,
            base_transfer_fee,
            trade_fee,
            creator_fee,
            base_decimals: info.base_decimals,
            quote_decimals,
            is_sol: quote_mint == WSOL_MINT,
            is_usdc: quote_mint == USDC_MINT,
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
            let n = ALT_TRADE_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
            let quote_unit = 10f64.powi(i32::from(quote_decimals.unwrap_or(0)));
            let base_unit = 10f64.powi(i32::from(info.base_decimals));
            info!(
                "[ALT TRADE #{n}] slot={slot} sig={sig} ix={ix} {side} mint={mint} quote={quote_mint} \
                 pool={pool} user={user} route={route} success={succ} src={src} \
                 quote_amt={qa:.6} tok={tok:.3} mc={mc:?} base_res={br:?} quote_res={qr:?} \
                 xfer_fee={xf:?} prio={prio:?} tip={tip:?} provider={prov:?} cu={cu:?}",
                n = n,
                slot = slot,
                sig = tx_signature,
                ix = trade_index,
                side = if is_buy { "BUY " } else { "SELL" },
                mint = mint,
                quote_mint = quote_mint,
                pool = pool_address,
                user = user,
                route = if *start_inner_pos > 0 { "cpi" } else { "direct" },
                succ = tx_succeeded,
                src = amount_source,
                qa = quote_amount as f64 / quote_unit,
                tok = token_amount as f64 / base_unit,
                mc = market_cap.map(|m| m as f64 / quote_unit),
                br = base_reserves,
                qr = quote_reserves,
                xf = base_transfer_fee,
                prio = priority_fee,
                tip = transfer_tip,
                prov = tip_provider,
                cu = compute_units_consumed,
            );
        } else if let Err(e) = sender.send(record).await {
            warn!("Alt trade channel closed; unable to persist trade: {:?}", e);
        }
        trade_index += 1;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashmap::DashMap;
    use serde_json::Value;
    use yellowstone_grpc_proto::prelude::{
        CompiledInstruction, InnerInstruction, InnerInstructions, Message, TokenBalance,
        Transaction, TransactionError, TransactionStatusMeta, UiTokenAmount,
    };

    const TX_CPMM_SWAP: &str =
        include_str!("../../fixtures/stonk/tx_cpmm_swap_base_input_2cY4Pc3j.json");
    const BASE_MINT: &str = "HrSjY8WztUJFavB2Joz5ojFLpMhfpNmxMyL57zUENcDg";
    const QUOTE_MINT: &str = "DoGEV7LASBkQbibMc5k5vKnTZoMg423GpJ5QtJEGfm7R";
    const POOL: &str = "DX8ykUKbMWduHgfQ5JnjfEec1oWaVbfuVRef6r1HRqHt";
    const SUPPLY: u64 = 1_000_000_000_000_000;

    fn info(cpmm_pool: Option<&str>, quote_decimals: Option<i16>) -> AltMintInfo {
        AltMintInfo {
            quote_mint: QUOTE_MINT.to_string(),
            total_supply: SUPPLY,
            base_decimals: 6,
            quote_decimals,
            cpmm_pool: cpmm_pool.map(str::to_string),
        }
    }

    fn key(s: &str) -> Vec<u8> {
        bs58::decode(s).into_vec().unwrap()
    }

    /// Convert the `json`-encoded RPC fixture (string account keys, base58
    /// instruction data, `loadedAddresses`) into the gRPC message shape.
    fn fixture_tx() -> SubscribeUpdateTransactionInfo {
        let v: Value = serde_json::from_str(TX_CPMM_SWAP).unwrap();
        let r = v.get("result").cloned().unwrap_or(v);
        let msg = &r["transaction"]["message"];
        let meta = &r["meta"];

        let account_keys = msg["accountKeys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| key(k.as_str().unwrap()))
            .collect();
        let compiled = |ix: &Value| CompiledInstruction {
            program_id_index: ix["programIdIndex"].as_u64().unwrap() as u32,
            accounts: ix["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_u64().unwrap() as u8)
                .collect(),
            data: bs58::decode(ix["data"].as_str().unwrap())
                .into_vec()
                .unwrap(),
        };
        let instructions = msg["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .map(compiled)
            .collect();
        let inner_instructions = meta["innerInstructions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| InnerInstructions {
                index: block["index"].as_u64().unwrap() as u32,
                instructions: block["instructions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|ix| {
                        let c = compiled(ix);
                        InnerInstruction {
                            program_id_index: c.program_id_index,
                            accounts: c.accounts,
                            data: c.data,
                            stack_height: ix["stackHeight"].as_u64().map(|h| h as u32),
                        }
                    })
                    .collect(),
            })
            .collect();
        let loaded = |k: &str| -> Vec<Vec<u8>> {
            meta["loadedAddresses"][k]
                .as_array()
                .map(|a| a.iter().map(|s| key(s.as_str().unwrap())).collect())
                .unwrap_or_default()
        };
        let balances = |k: &str| -> Vec<TokenBalance> {
            meta[k]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| {
                    let u = &b["uiTokenAmount"];
                    TokenBalance {
                        account_index: b["accountIndex"].as_u64().unwrap() as u32,
                        mint: b["mint"].as_str().unwrap().to_string(),
                        owner: b["owner"].as_str().unwrap_or_default().to_string(),
                        program_id: b["programId"].as_str().unwrap_or_default().to_string(),
                        ui_token_amount: Some(UiTokenAmount {
                            ui_amount: u["uiAmount"].as_f64().unwrap_or(0.0),
                            decimals: u["decimals"].as_u64().unwrap() as u32,
                            amount: u["amount"].as_str().unwrap().to_string(),
                            ui_amount_string: u["uiAmountString"].as_str().unwrap().to_string(),
                        }),
                    }
                })
                .collect()
        };
        let log_messages = meta["logMessages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap().to_string())
            .collect();

        SubscribeUpdateTransactionInfo {
            signature: bs58::decode(r["transaction"]["signatures"][0].as_str().unwrap())
                .into_vec()
                .unwrap(),
            transaction: Some(Transaction {
                message: Some(Message {
                    account_keys,
                    instructions,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            meta: Some(TransactionStatusMeta {
                err: None,
                inner_instructions,
                loaded_writable_addresses: loaded("writable"),
                loaded_readonly_addresses: loaded("readonly"),
                pre_token_balances: balances("preTokenBalances"),
                post_token_balances: balances("postTokenBalances"),
                log_messages,
                compute_units_consumed: meta["computeUnitsConsumed"].as_u64(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    async fn run(tx: &SubscribeUpdateTransactionInfo, map: &DashMap<String, AltMintInfo>) -> Vec<AltTradeRecord> {
        let (sender, mut receiver) = mpsc::channel(16);
        process_cpmm_tx(tx, 446_404_878, map, &sender, false)
            .await
            .unwrap();
        drop(sender);
        let mut out = Vec::new();
        while let Some(r) = receiver.recv().await {
            out.push(r);
        }
        out
    }

    /// The fixture's `swap_base_input` instruction, wherever it sits.
    fn find_swap(tx: &SubscribeUpdateTransactionInfo) -> CompiledInstruction {
        let msg = tx.transaction.as_ref().unwrap().message.as_ref().unwrap();
        if let Some(ix) = msg
            .instructions
            .iter()
            .find(|i| i.data.starts_with(&CPMM_SWAP_BASE_INPUT_DISCRIMINATOR))
        {
            return ix.clone();
        }
        tx.meta
            .as_ref()
            .unwrap()
            .inner_instructions
            .iter()
            .flat_map(|b| b.instructions.iter())
            .find(|i| i.data.starts_with(&CPMM_SWAP_BASE_INPUT_DISCRIMINATOR))
            .map(|i| CompiledInstruction {
                program_id_index: i.program_id_index,
                accounts: i.accounts.clone(),
                data: i.data.clone(),
            })
            .expect("swap instruction in fixture")
    }

    /// Live direct sell (base in) in a tx that uses an ALT and routes a second
    /// hop through Raydium CLMM: the SwapEvent comes from the logs (the CLMM
    /// SwapEvent with the same discriminator is rejected), the pool is the
    /// derived PDA.
    #[tokio::test]
    async fn records_live_cpmm_sell_from_fixture() {
        let tx = fixture_tx();
        let map = DashMap::new();
        map.insert(BASE_MINT.to_string(), info(None, None));
        let records = run(&tx, &map).await;
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert!(r.tx_signature.starts_with("2cY4Pc3j"), "{}", r.tx_signature);
        assert_eq!(r.ix_index, 0);
        assert_eq!(r.platform, "stonk_fun");
        assert_eq!(r.venue, "raydium_cpmm");
        assert_eq!(r.pool_address, POOL);
        assert_eq!(r.mint_address, BASE_MINT);
        assert_eq!(r.quote_mint_address, QUOTE_MINT);
        assert_eq!(r.instruction_type, "swap_base_input");
        assert_eq!(r.amount_source, "event");
        assert!(!r.is_cpi);
        assert!(!r.is_buy);
        assert!(r.success);
        assert_eq!(r.token_amount, 4_098_714_943_283);
        assert_eq!(r.quote_amount, 120_823_636_524);
        assert_eq!(r.base_reserves, Some(269_492_128_397_846));
        assert_eq!(r.quote_reserves, Some(7_923_428_228_813));
        assert_eq!(r.base_transfer_fee, Some(41_401_161_044));
        assert_eq!(r.trade_fee, Some(10_246_787_359));
        assert_eq!(r.creator_fee, Some(1_220_440_773));
        let expected_mc = 7_923_428_228_813u128 * SUPPLY as u128 / 269_492_128_397_846u128;
        assert_eq!(r.market_cap, Some(expected_mc as u64));
        // Quote decimals learned from the tx since alt_mints had none.
        assert_eq!(r.quote_decimals, Some(8));
        assert_eq!(r.base_decimals, 6);
        assert!(!r.is_sol);
        assert!(!r.is_usdc);
        assert_eq!(r.slot, 446_404_878);
        assert!(r.compute_units_consumed.is_some());
        // Swap payer (ix account 0) == the tx fee payer in this fixture.
        assert_eq!(r.user_pubkey, "5yeRmwgdP28xfqsAPSe16hcETkvhzpWhBwzH9g5fAdiM");
    }

    #[tokio::test]
    async fn alt_mints_quote_decimals_take_precedence_and_recorded_pool_is_trusted() {
        let tx = fixture_tx();
        let map = DashMap::new();
        map.insert(BASE_MINT.to_string(), info(Some(POOL), Some(8)));
        let records = run(&tx, &map).await;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].quote_decimals, Some(8));
    }

    #[tokio::test]
    async fn untracked_mint_wrong_quote_and_wrong_pool_are_dropped() {
        let tx = fixture_tx();

        // Not in alt_mints.
        assert!(run(&tx, &DashMap::new()).await.is_empty());

        // Tracked, but the launch quote differs from the pool's other leg.
        let map = DashMap::new();
        map.insert(
            BASE_MINT.to_string(),
            AltMintInfo {
                quote_mint: WSOL_MINT.to_string(),
                ..info(None, None)
            },
        );
        assert!(run(&tx, &map).await.is_empty());

        // Right pair, but pool_state is not the migration pool PDA and not
        // the recorded pool.
        let mut tx2 = fixture_tx();
        {
            let meta = tx2.meta.as_mut().unwrap();
            let msg = tx2.transaction.as_mut().unwrap().message.as_mut().unwrap();
            let mut full: Vec<Vec<u8>> = msg.account_keys.clone();
            full.extend(meta.loaded_writable_addresses.iter().cloned());
            full.extend(meta.loaded_readonly_addresses.iter().cloned());
            let pool_idx = full.iter().position(|k| *k == key(POOL)).unwrap();
            let fake = Pubkey::new_unique().to_bytes().to_vec();
            if pool_idx < msg.account_keys.len() {
                msg.account_keys[pool_idx] = fake;
            } else {
                let w = pool_idx - msg.account_keys.len();
                if w < meta.loaded_writable_addresses.len() {
                    meta.loaded_writable_addresses[w] = fake;
                } else {
                    let r = w - meta.loaded_writable_addresses.len();
                    meta.loaded_readonly_addresses[r] = fake;
                }
            }
        }
        let map = DashMap::new();
        map.insert(BASE_MINT.to_string(), info(None, None));
        assert!(run(&tx2, &map).await.is_empty());
        // …unless strategy-data recorded exactly that pool from a migration.
        let fake_pool = {
            let meta = tx2.meta.as_ref().unwrap();
            let msg = tx2.transaction.as_ref().unwrap().message.as_ref().unwrap();
            let mut full: Vec<Vec<u8>> = msg.account_keys.clone();
            full.extend(meta.loaded_writable_addresses.iter().cloned());
            full.extend(meta.loaded_readonly_addresses.iter().cloned());
            let swap = find_swap(&tx2);
            bs58::encode(&full[swap.accounts[CPMM_SWAP_POOL_STATE_IX_POS] as usize]).into_string()
        };
        let map = DashMap::new();
        map.insert(BASE_MINT.to_string(), info(Some(&fake_pool), None));
        let records = run(&tx2, &map).await;
        assert_eq!(records.len(), 1);
        // The event no longer matches by pool id (it still names the real
        // pool) and emit_cpi is absent, so the vault fallback kicks in.
        assert_eq!(records[0].amount_source, "vault_balances");
        assert_eq!(records[0].token_amount, 4_098_714_943_283);
        assert_eq!(records[0].quote_amount, 120_823_636_524);
    }

    #[tokio::test]
    async fn failed_tx_keeps_requested_amounts_and_no_market_cap() {
        let mut tx = fixture_tx();
        tx.meta.as_mut().unwrap().err = Some(TransactionError { err: vec![1] });
        let map = DashMap::new();
        map.insert(BASE_MINT.to_string(), info(None, Some(8)));
        let records = run(&tx, &map).await;
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert!(!r.success);
        assert_eq!(r.amount_source, "instruction_request");
        assert!(!r.is_buy);
        assert!(r.market_cap.is_none());
        assert!(r.base_reserves.is_none());
        // swap_base_input(amount_in, minimum_amount_out): base is the input leg.
        let swap = find_swap(&tx);
        let amount_in = u64::from_le_bytes(swap.data[8..16].try_into().unwrap());
        let min_out = u64::from_le_bytes(swap.data[16..24].try_into().unwrap());
        assert_eq!(r.token_amount, amount_in);
        assert_eq!(r.quote_amount, min_out);
        // The pre-transfer-fee amount the wallet was debited.
        assert_eq!(amount_in, 4_098_714_943_283 + 41_401_161_044);
    }

    /// Two swaps on the same pool in one tx — a CPI one under the aggregator
    /// ix that precedes the fixture's top-level swap — must each get their
    /// own event, matched in execution order (CPI first, top-level second)
    /// even though `get_program_instructions` lists top-level calls first.
    #[tokio::test]
    async fn multi_swap_tx_matches_events_in_execution_order() {
        let mut tx = fixture_tx();
        let swap_ix = find_swap(&tx);
        let msg = tx.transaction.as_ref().unwrap().message.as_ref().unwrap();
        let top_idx = msg
            .instructions
            .iter()
            .position(|i| i.data.starts_with(&CPMM_SWAP_BASE_INPUT_DISCRIMINATOR))
            .unwrap();
        assert!(top_idx > 0);
        let parent = (top_idx - 1) as u32;

        let meta = tx.meta.as_mut().unwrap();
        // CPI copy of the swap under the preceding top-level instruction.
        let block = meta
            .inner_instructions
            .iter_mut()
            .find(|b| b.index == parent)
            .expect("preceding ix has an inner block");
        block.instructions.push(InnerInstruction {
            program_id_index: swap_ix.program_id_index,
            accounts: swap_ix.accounts.clone(),
            data: swap_ix.data.clone(),
            stack_height: Some(2),
        });
        // Its SwapEvent is the fixture's existing log line (executed first);
        // append a second one with different amounts for the top-level swap.
        let first = cpmm_swap_events_from_logs(&meta.log_messages).remove(0);
        let mut second = CPMM_SWAP_EVENT_DISC.to_vec();
        let mut body = vec![0u8; 162];
        body[..32].copy_from_slice(&key(POOL));
        body[32..40].copy_from_slice(&(first.input_vault_before + first.input_amount).to_le_bytes());
        body[40..48].copy_from_slice(&(first.output_vault_before - first.output_amount).to_le_bytes());
        body[48..56].copy_from_slice(&1_000u64.to_le_bytes());
        body[56..64].copy_from_slice(&30u64.to_le_bytes());
        body[80] = 1;
        body[81..113].copy_from_slice(&key(BASE_MINT));
        body[113..145].copy_from_slice(&key(QUOTE_MINT));
        second.extend(body);
        use base64::Engine;
        meta.log_messages.push(format!(
            "Program data: {}",
            base64::engine::general_purpose::STANDARD.encode(&second)
        ));

        let map = DashMap::new();
        map.insert(BASE_MINT.to_string(), info(None, Some(8)));
        let records = run(&tx, &map).await;
        assert_eq!(records.len(), 2);
        assert!(records[0].is_cpi);
        assert_eq!(records[0].ix_index, 0);
        assert_eq!(records[0].amount_source, "event");
        assert_eq!(records[0].token_amount, 4_098_714_943_283);
        assert!(!records[1].is_cpi);
        assert_eq!(records[1].ix_index, 1);
        assert_eq!(records[1].amount_source, "event");
        assert_eq!(records[1].token_amount, 1_000);
        assert_eq!(records[1].quote_amount, 30);
        assert_eq!(
            records[1].base_reserves,
            Some(first.input_vault_before + first.input_amount + 1_000)
        );
    }
}
