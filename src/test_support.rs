//! Test-only helpers shared across modules.

use serde_json::Value;
use yellowstone_grpc_proto::geyser::SubscribeUpdateTransactionInfo;
use yellowstone_grpc_proto::prelude::{
    CompiledInstruction, InnerInstruction, InnerInstructions, Message, TokenBalance, Transaction,
    TransactionError, TransactionStatusMeta, UiTokenAmount,
};

fn b58(s: &str) -> Vec<u8> {
    bs58::decode(s).into_vec().unwrap()
}

/// Convert a `getTransaction` RPC response (`json` encoding: string account
/// keys, base58 instruction data, `loadedAddresses`) into the gRPC message
/// the handlers receive. Works for legacy, v0 and v1 transactions. Returns
/// the tx and its slot.
pub fn rpc_json_to_grpc(json: &str) -> (SubscribeUpdateTransactionInfo, u64) {
    let v: Value = serde_json::from_str(json).unwrap();
    let t = v.get("result").cloned().unwrap_or(v);
    let slot = t["slot"].as_u64().unwrap();
    let msg = &t["transaction"]["message"];
    let meta = &t["meta"];

    let compiled = |ix: &Value| CompiledInstruction {
        program_id_index: ix["programIdIndex"].as_u64().unwrap() as u32,
        accounts: ix["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_u64().unwrap() as u8)
            .collect(),
        data: b58(ix["data"].as_str().unwrap()),
    };
    let instructions = msg["instructions"]
        .as_array()
        .unwrap()
        .iter()
        .map(compiled)
        .collect();
    let inner_instructions = meta["innerInstructions"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .map(|g| InnerInstructions {
                    index: g["index"].as_u64().unwrap() as u32,
                    instructions: g["instructions"]
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
                .collect()
        })
        .unwrap_or_default();
    let loaded = |k: &str| -> Vec<Vec<u8>> {
        meta["loadedAddresses"][k]
            .as_array()
            .map(|a| a.iter().map(|s| b58(s.as_str().unwrap())).collect())
            .unwrap_or_default()
    };
    let token_balances = |k: &str| -> Vec<TokenBalance> {
        meta[k]
            .as_array()
            .map(|a| {
                a.iter()
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
                                ui_amount_string: u["uiAmountString"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                            }),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let balances = |k: &str| -> Vec<u64> {
        meta[k]
            .as_array()
            .map(|a| a.iter().map(|x| x.as_u64().unwrap()).collect())
            .unwrap_or_default()
    };

    let tx = SubscribeUpdateTransactionInfo {
        signature: b58(t["transaction"]["signatures"][0].as_str().unwrap()),
        transaction: Some(Transaction {
            message: Some(Message {
                account_keys: msg["accountKeys"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|k| b58(k.as_str().unwrap()))
                    .collect(),
                instructions,
                ..Default::default()
            }),
            ..Default::default()
        }),
        meta: Some(TransactionStatusMeta {
            err: (!meta["err"].is_null()).then(|| TransactionError { err: vec![1] }),
            fee: meta["fee"].as_u64().unwrap_or(0),
            pre_balances: balances("preBalances"),
            post_balances: balances("postBalances"),
            inner_instructions,
            pre_token_balances: token_balances("preTokenBalances"),
            post_token_balances: token_balances("postTokenBalances"),
            loaded_writable_addresses: loaded("writable"),
            loaded_readonly_addresses: loaded("readonly"),
            compute_units_consumed: meta["computeUnitsConsumed"].as_u64(),
            ..Default::default()
        }),
        ..Default::default()
    };
    (tx, slot)
}
