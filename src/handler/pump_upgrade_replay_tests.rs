//! Replays REAL transactions of the 2026-10-08 Pump / PumpSwap upgrade
//! (`fixtures/pump/upgrade_2026_10/`, see its `manifest.json`; copied from
//! strategy-data) through the AMM handler and checks every record against
//! ground truth decoded independently from the raw RPC JSON:
//!
//! * one record per PumpSwap `BuyEvent` / `SellEvent` on a canonical
//!   pool (any quote mint, non-WSOL base), in event order, with the event's pool, user, side and
//!   base amount; the pair is recovered from the pool's own token-balance
//!   rows and the canonical-pool PDA, not from instruction accounts;
//! * `instruction_type` is the IDL name of the event's parent instruction,
//!   except under `multi_hop_swap`, where a hop on a coin whose balance the
//!   fee payer didn't move is `multi_hop_swap_routed`;
//! * no record from pump curve hops, synthetic migrations or pool creation.

use std::collections::HashMap;
use std::str::FromStr;

use dashmap::DashSet;
use serde_json::Value;
use solana_program::pubkey::Pubkey;
use tokio::sync::mpsc;
use yellowstone_grpc_proto::prelude::TransactionError;

use super::amm_handler::process_pump_swap_tx as process;
use crate::database::TradeRecord;
use crate::test_support::rpc_json_to_grpc;
use crate::utils::{
    AMM_MULTI_HOP_SWAP_DISCRIMINATOR, PUMP_PROGRAM_ID, PUMP_SWAP_BUY_EVENT_DISC,
    PUMP_SWAP_PROGRAM_ID, PUMP_SWAP_SELL_EVENT_DISC, WSOL_MINT,
};

const PUMP_AMM_IDL: &str = include_str!("../../fixtures/pump/pump_amm_idl.json");

fn load(name: &str) -> String {
    let path = format!(
        "{}/fixtures/pump/upgrade_2026_10/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn b58(s: &str) -> Vec<u8> {
    bs58::decode(s).into_vec().unwrap()
}

fn u64_at(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}

fn key_at(d: &[u8], o: usize) -> String {
    bs58::encode(&d[o..o + 32]).into_string()
}

/// PumpSwap instruction name by discriminator, straight from the IDL.
fn idl_names() -> HashMap<Vec<u8>, String> {
    let idl: Value = serde_json::from_str(PUMP_AMM_IDL).unwrap();
    idl["instructions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ix| {
            let disc = ix["discriminator"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b.as_u64().unwrap() as u8)
                .collect();
            (disc, ix["name"].as_str().unwrap().to_string())
        })
        .collect()
}

/// Independent re-derivation of the canonical pool PDA.
fn canonical(base: &str, quote: &str) -> String {
    let pump = Pubkey::from_str(PUMP_PROGRAM_ID).unwrap();
    let amm = Pubkey::from_str(PUMP_SWAP_PROGRAM_ID).unwrap();
    let base = Pubkey::from_str(base).unwrap();
    let quote = Pubkey::from_str(quote).unwrap();
    let (authority, _) = Pubkey::find_program_address(&[b"pool-authority", base.as_ref()], &pump);
    Pubkey::find_program_address(
        &[
            b"pool",
            &0u16.to_le_bytes(),
            authority.as_ref(),
            base.as_ref(),
            quote.as_ref(),
        ],
        &amm,
    )
    .0
    .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Expected {
    pool: String,
    mint: String,
    quote_mint: String,
    user: String,
    is_buy: bool,
    token_amount: u64,
    instruction_type: String,
}

struct Truth {
    expected: Vec<Expected>,
    /// Every pool event, recordable or not: (pool, base, quote, is_buy).
    pool_events: Vec<(String, String, String, bool)>,
    multi_hop: bool,
}

fn truth(json: &str) -> Truth {
    let names = idl_names();
    let v: Value = serde_json::from_str(json).unwrap();
    let t = v.get("result").cloned().unwrap_or(v);
    let meta = &t["meta"];
    let keys: Vec<String> = t["transaction"]["message"]["accountKeys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap().to_string())
        .chain(["writable", "readonly"].iter().flat_map(|k| {
            meta["loadedAddresses"][k]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.as_str().unwrap().to_string())
        }))
        .collect();
    let payer = keys[0].clone();

    let rows = |k: &str| -> Vec<(String, String, i128)> {
        meta[k]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                (
                    b["owner"].as_str().unwrap_or_default().to_string(),
                    b["mint"].as_str().unwrap().to_string(),
                    b["uiTokenAmount"]["amount"]
                        .as_str()
                        .unwrap()
                        .parse()
                        .unwrap(),
                )
            })
            .collect()
    };
    let (pre, post) = (rows("preTokenBalances"), rows("postTokenBalances"));
    let payer_balance = |rows: &[(String, String, i128)], mint: &str| -> i128 {
        rows.iter()
            .filter(|(o, m, _)| *o == payer && m == mint)
            .map(|(_, _, a)| a)
            .sum()
    };
    let pool_mints = |pool: &str| -> Vec<String> {
        let mut m: Vec<String> = post
            .iter()
            .filter(|(o, _, _)| o == pool)
            .map(|(_, m, _)| m.clone())
            .collect();
        m.sort();
        m.dedup();
        m
    };

    let mut flat: Vec<(String, Vec<u8>, u32)> = Vec::new(); // (program, data, stack)
    for (i, ix) in t["transaction"]["message"]["instructions"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        flat.push((
            keys[ix["programIdIndex"].as_u64().unwrap() as usize].clone(),
            b58(ix["data"].as_str().unwrap()),
            1,
        ));
        for block in meta["innerInstructions"].as_array().unwrap() {
            if block["index"].as_u64().unwrap() as usize != i {
                continue;
            }
            for ii in block["instructions"].as_array().unwrap() {
                flat.push((
                    keys[ii["programIdIndex"].as_u64().unwrap() as usize].clone(),
                    b58(ii["data"].as_str().unwrap()),
                    ii["stackHeight"].as_u64().unwrap() as u32,
                ));
            }
        }
    }

    let mut out = Truth {
        expected: vec![],
        pool_events: vec![],
        multi_hop: false,
    };
    for (j, (program, data, stack)) in flat.iter().enumerate() {
        if program != PUMP_SWAP_PROGRAM_ID {
            continue;
        }
        if names.get(&data[..8.min(data.len())]).map(String::as_str) == Some("multi_hop_swap") {
            out.multi_hop = true;
        }
        if data.len() < 16 {
            continue;
        }
        let ev = &data[8..];
        let is_buy = if ev[..8] == PUMP_SWAP_BUY_EVENT_DISC {
            true
        } else if ev[..8] == PUMP_SWAP_SELL_EVENT_DISC {
            false
        } else {
            continue;
        };
        let pool = key_at(ev, 120);
        let user = key_at(ev, 152);
        let mints = pool_mints(&pool);
        assert_eq!(mints.len(), 2, "pool {pool} vault rows");
        let (base, quote) = if canonical(&mints[0], &mints[1]) == pool {
            (mints[0].clone(), mints[1].clone())
        } else {
            assert_eq!(
                canonical(&mints[1], &mints[0]),
                pool,
                "non-canonical pool {pool}"
            );
            (mints[1].clone(), mints[0].clone())
        };
        out.pool_events
            .push((pool.clone(), base.clone(), quote.clone(), is_buy));
        // The instruction that emitted it: the nearest PumpSwap ix one level up.
        let parent = flat[..j]
            .iter()
            .rev()
            .find(|(p, _, s)| *s == stack - 1 && p == PUMP_SWAP_PROGRAM_ID)
            .map(|(_, d, _)| names[&d[..8].to_vec()].clone())
            .unwrap();
        let instruction_type = if parent == "multi_hop_swap" {
            if payer_balance(&pre, &base) == payer_balance(&post, &base) {
                "multi_hop_swap_routed".to_string()
            } else {
                "multi_hop_swap".to_string()
            }
        } else {
            parent
        };
        if base != WSOL_MINT {
            out.expected.push(Expected {
                pool,
                mint: base,
                quote_mint: quote,
                user,
                is_buy,
                token_amount: u64_at(ev, 16),
                instruction_type,
            });
        }
    }
    out
}

fn all_fixtures() -> Vec<String> {
    let manifest: Value = serde_json::from_str(&load("manifest")).unwrap();
    let mut names: Vec<String> = manifest.as_object().unwrap().keys().cloned().collect();
    names.sort();
    names
}

async fn run(
    tx: yellowstone_grpc_proto::geyser::SubscribeUpdateTransactionInfo,
    slot: u64,
    whitelist: &DashSet<String>,
) -> Vec<TradeRecord> {
    let (sender, mut receiver) = mpsc::channel(64);
    process(tx, slot, whitelist, &sender, false).await.unwrap();
    drop(sender);
    let mut out = Vec::new();
    while let Some(r) = receiver.recv().await {
        out.push(r);
    }
    out
}

/// Whitelist every mint the tx mentions, so only the intentional pool
/// filters decide what is recorded.
fn whitelist_all(json: &str) -> DashSet<String> {
    let v: Value = serde_json::from_str(json).unwrap();
    let t = v.get("result").cloned().unwrap_or(v);
    let set = DashSet::new();
    for k in ["preTokenBalances", "postTokenBalances"] {
        for b in t["meta"][k].as_array().unwrap() {
            set.insert(b["mint"].as_str().unwrap().to_string());
        }
    }
    set
}

#[tokio::test]
async fn real_upgrade_transactions_record_exactly_the_chain_pool_trades() {
    let mut total = 0;
    let mut routed = 0;
    let mut by_kind: HashMap<String, usize> = HashMap::new();
    for name in all_fixtures() {
        let json = load(&name);
        let truth = truth(&json);
        let (tx, slot) = rpc_json_to_grpc(&json);
        let records = run(tx, slot, &whitelist_all(&json)).await;
        let got: Vec<Expected> = records
            .iter()
            .map(|r| Expected {
                pool: r.pool_address.clone(),
                mint: r.mint_address.clone(),
                quote_mint: r.quote_mint_address.clone(),
                user: r.user_pubkey.clone(),
                is_buy: r.is_buy,
                token_amount: r.token_amount,
                instruction_type: r.instruction_type.clone(),
            })
            .collect();
        assert_eq!(got, truth.expected, "{name}");
        for (i, r) in records.iter().enumerate() {
            assert_eq!(r.ix_index, i as i32, "{name}");
            assert!(r.success, "{name}");
            assert_eq!(r.amount_source, "event", "{name}");
            assert!(r.market_cap.is_some_and(|mc| mc > 0), "{name}");
            assert_eq!(r.slot, slot as i64);
        }
        total += records.len();
        routed += records
            .iter()
            .filter(|r| r.instruction_type == "multi_hop_swap_routed")
            .count();
        for r in &records {
            *by_kind.entry(r.instruction_type.clone()).or_default() += 1;
        }
        if !truth.multi_hop {
            continue;
        }
        // A multi-hop tx must never record a pool event twice.
        assert!(records.len() <= truth.pool_events.len(), "{name}");
    }
    // Pinned so the comparison above can't pass vacuously: 2 mainnet v2
    // trades, and 8 pool hops from the 4 multi-hop routes (pool→curve buy,
    // curve→pool sell, aggregator-wrapped pool→pool buy and sell). The 4
    // SOL-quoted hops are middle hops (routed); the 4 end-coin hops trade on
    // pools quoted in the middle pump coin, recorded like any other quote.
    assert_eq!(total, 10, "{by_kind:?}");
    assert_eq!(routed, 4, "{by_kind:?}");
    assert_eq!(by_kind.get("multi_hop_swap"), Some(&4));
    assert_eq!(by_kind.get("buy_exact_quote_in_v2"), Some(&1));
    assert_eq!(by_kind.get("sell_v2"), Some(&1));
}

/// The same multi-hop routes, failed: no events, so every eligible hop is
/// recorded from the instruction with its side inferred from the route —
/// which must equal the side the successful tx's events show.
#[tokio::test]
async fn failed_multi_hop_routes_record_each_hop_from_the_instruction() {
    let mut checked = 0;
    for name in all_fixtures() {
        let json = load(&name);
        let truth = truth(&json);
        if !truth.multi_hop {
            continue;
        }
        let (mut tx, slot) = rpc_json_to_grpc(&json);
        let meta = tx.meta.as_mut().unwrap();
        meta.err = Some(TransactionError { err: vec![1] });
        // A failed route emits nothing; keep only an aggregator's CPI into
        // multi_hop_swap itself, which is how a wrapped route is found.
        for block in &mut meta.inner_instructions {
            block
                .instructions
                .retain(|ix| ix.data.get(..8) == Some(AMM_MULTI_HOP_SWAP_DISCRIMINATOR.as_slice()));
        }
        meta.post_token_balances = meta.pre_token_balances.clone();
        let records = run(tx, slot, &whitelist_all(&json)).await;

        let expected: Vec<(String, bool, String)> = truth
            .expected
            .iter()
            .map(|e| (e.mint.clone(), e.is_buy, e.instruction_type.clone()))
            .collect();
        let got: Vec<(String, bool, String)> = records
            .iter()
            .map(|r| (r.mint_address.clone(), r.is_buy, r.instruction_type.clone()))
            .collect();
        assert_eq!(got, expected, "{name}");
        for r in &records {
            assert!(!r.success);
            assert_eq!(r.amount_source, "instruction_request");
            assert_eq!(r.market_cap, None);
        }
        checked += records.len();
    }
    assert_eq!(checked, 8);
}

/// `migrate_v2` (incl. after a synthetic migration) opens exactly the
/// canonical pool the handler filters on, so post-migration trades of a
/// synthetically migrated coin are recorded.
#[test]
fn migrate_v2_creates_the_canonical_pool() {
    let names = idl_names();
    let mut seen = 0;
    for name in all_fixtures()
        .into_iter()
        .filter(|n| n.contains("migrate_v2"))
    {
        let v: Value = serde_json::from_str(&load(&name)).unwrap();
        let t = v.get("result").cloned().unwrap_or(v);
        let keys: Vec<&str> = t["transaction"]["message"]["accountKeys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap())
            .collect();
        for block in t["meta"]["innerInstructions"].as_array().unwrap() {
            for ix in block["instructions"].as_array().unwrap() {
                let data = b58(ix["data"].as_str().unwrap());
                if keys[ix["programIdIndex"].as_u64().unwrap() as usize] != PUMP_SWAP_PROGRAM_ID
                    || names.get(&data[..8.min(data.len())]).map(String::as_str)
                        != Some("create_pool")
                {
                    continue;
                }
                let acc = |i: usize| keys[ix["accounts"][i].as_u64().unwrap() as usize];
                assert_eq!(canonical(acc(3), acc(4)), acc(0), "{name}");
                seen += 1;
            }
        }
    }
    assert_eq!(seen, 3);
}

#[test]
fn manifest_lists_every_fixture() {
    let mut on_disk: Vec<String> = std::fs::read_dir(format!(
        "{}/fixtures/pump/upgrade_2026_10",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
    .map(|e| e.unwrap().file_name().into_string().unwrap())
    .filter_map(|f| f.strip_suffix(".json").map(str::to_string))
    .filter(|f| f != "manifest")
    .collect();
    on_disk.sort();
    assert_eq!(on_disk, all_fixtures());
}
