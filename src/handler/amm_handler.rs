use anyhow::Result;
use chrono::Utc;
use dashmap::DashSet;
use log::{error, warn};
use solana_program::pubkey::Pubkey;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use yellowstone_grpc_proto::geyser::SubscribeUpdateTransactionInfo;

use crate::database::TradeRecord;
use crate::utils::{
    PUMP_SWAP_PROGRAM_ID, WSOL_MINT, extract_instruction_type, extract_pool_reserves_from_data,
    extract_sol_volume, extract_transaction_amounts, get_instruction_blocks, get_market_cap_in_sol,
    get_mint_from_instruction, get_program_instructions, get_user,
};

/// Creates the gRPC handler closure that parses Pump Swap AMM transactions,
/// filters by whitelisted mints, and sends trade records to the batch inserter.
pub fn create_amm_handler(
    whitelist: Arc<DashSet<String>>,
    sender: mpsc::Sender<TradeRecord>,
) -> impl Fn(SubscribeUpdateTransactionInfo, u64) -> Result<()> + Clone + Send + Sync + 'static {
    let handler = Arc::new(move |tx_data: SubscribeUpdateTransactionInfo, slot: u64| {
        let whitelist = whitelist.clone();
        let sender = sender.clone();
        tokio::spawn(async move {
            if let Err(e) = process_pump_swap_tx(tx_data, slot, &whitelist, &sender) {
                error!("Error processing AMM transaction: {:?}", e);
            }
        });
        Ok(())
    });

    move |tx_data, slot| handler(tx_data, slot)
}

/// Parse a Pump Swap transaction, extract trade data, check whitelist, and send to channel.
fn process_pump_swap_tx(
    tx_data: SubscribeUpdateTransactionInfo,
    slot: u64,
    whitelist: &DashSet<String>,
    sender: &mpsc::Sender<TradeRecord>,
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

    // Build full account list including loaded addresses
    let mut full_accounts = account_keys.clone();
    full_accounts.extend(meta.loaded_writable_addresses.iter().cloned());
    full_accounts.extend(meta.loaded_readonly_addresses.iter().cloned());

    // Get all Pump Swap program instructions (top-level + CPI)
    let all_instrs = get_program_instructions(msg, meta, &full_accounts, &program_id);
    let log_blocks = get_instruction_blocks(&meta.log_messages, PUMP_SWAP_PROGRAM_ID);

    let tx_signature = bs58::encode(&tx_data.signature).into_string();
    let now = Utc::now();

    for (instr_idx, (instr, _parent_idx)) in all_instrs.iter().enumerate() {
        // Extract instruction type from logs
        let block_opt = log_blocks.get(instr_idx);
        let instruction_type = block_opt.and_then(|block| extract_instruction_type(block));

        // Determine buy vs sell
        let is_buy = matches!(
            instruction_type.as_deref(),
            Some("Buy") | Some("BuyExactQuoteIn")
        );
        let is_sell = instruction_type.as_deref() == Some("Sell");

        if !is_buy && !is_sell {
            continue;
        }

        // Skip non-SOL pools: ensure WSOL is one of the two mints
        // IDL layout: accounts[3] = base_mint, accounts[4] = quote_mint
        if instr.accounts.len() > 4 {
            let base_mint_idx = instr.accounts[3] as usize;
            let quote_mint_idx = instr.accounts[4] as usize;
            let base_is_wsol = base_mint_idx < full_accounts.len()
                && bs58::encode(&full_accounts[base_mint_idx]).into_string() == WSOL_MINT;
            let quote_is_wsol = quote_mint_idx < full_accounts.len()
                && bs58::encode(&full_accounts[quote_mint_idx]).into_string() == WSOL_MINT;
            if !base_is_wsol && !quote_is_wsol {
                continue;
            }
        }

        // Extract the token mint
        let mint = match get_mint_from_instruction(instr, &full_accounts, meta) {
            Ok(Some(m)) => m,
            Ok(None) => continue,
            Err(_) => continue,
        };

        // Filter: only process whitelisted mints
        if !whitelist.contains(&mint) {
            continue;
        }

        // Extract program data from "Program data: " log line
        let program_data = block_opt.and_then(|block| {
            block.iter().find_map(|log| {
                if log.starts_with("Program data: ") {
                    Some(log.trim_start_matches("Program data: ").trim())
                } else {
                    None
                }
            })
        });

        let Some(data) = program_data else {
            continue;
        };

        // Extract pool reserves
        let (pool_base, pool_quote) = extract_pool_reserves_from_data(data)?;
        let (Some(base_reserves), Some(quote_reserves)) = (pool_base, pool_quote) else {
            continue;
        };

        // Extract SOL volume
        let (buy_vol, sell_vol) = extract_sol_volume(data)?;
        let sol_volume = if is_buy {
            buy_vol.unwrap_or(0)
        } else {
            sell_vol.unwrap_or(0)
        };

        // Extract token amount
        let token_amount = extract_transaction_amounts(data)?.unwrap_or(0);

        // Get user (signer)
        let user = get_user(&full_accounts);

        // Calculate market cap
        let market_cap = get_market_cap_in_sol(
            base_reserves,
            quote_reserves,
            token_amount,
            sol_volume,
            is_buy,
        );

        let record = TradeRecord {
            tx_signature: tx_signature.clone(),
            mint_address: mint,
            user_pubkey: user,
            is_buy,
            token_amount: token_amount as i64,
            sol_amount: sol_volume as i64,
            market_cap: Some(market_cap as i64),
            slot: slot as i64,
            created_at: now,
        };

        if let Err(e) = sender.try_send(record) {
            warn!("Trade channel full or closed, dropping trade: {:?}", e);
        }
    }

    Ok(())
}
