use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use dashmap::DashSet;
use solana_sdk::pubkey::Pubkey;
use yellowstone_grpc_proto::prelude::{CompiledInstruction, Message, TransactionStatusMeta};

use super::WSOL_MINT;

/// Helper to get the token mint from the instruction by finding the non-WSOL
/// account whose token balance changed.
pub fn get_mint_from_instruction(
    instr: &CompiledInstruction,
    account_keys: &[Vec<u8>],
    meta: &TransactionStatusMeta,
) -> Result<Option<String>> {
    let mints = DashSet::new();
    for &idx in &instr.accounts {
        let idx = idx as usize;
        if idx >= account_keys.len() {
            continue;
        }

        let pre_balance = meta
            .pre_token_balances
            .iter()
            .find(|b| b.account_index == idx as u32);

        let post_balance = meta
            .post_token_balances
            .iter()
            .find(|b| b.account_index == idx as u32);

        if let (Some(pre), Some(post)) = (pre_balance, post_balance) {
            let pre_amount = pre
                .ui_token_amount
                .as_ref()
                .and_then(|a| a.amount.parse::<u64>().ok())
                .unwrap_or(0);
            let post_amount = post
                .ui_token_amount
                .as_ref()
                .and_then(|a| a.amount.parse::<u64>().ok())
                .unwrap_or(0);
            if pre_amount != post_amount {
                mints.insert(post.mint.clone());
            }
        }
    }
    if mints.len() == 1 {
        Ok(Some(mints.into_iter().next().unwrap()))
    } else if mints.len() == 2 {
        let non_sol_mint = mints
            .iter()
            .find(|mint| mint.as_str() != WSOL_MINT)
            .map(|mint| mint.clone());

        if let Some(mint) = non_sol_mint {
            Ok(Some(mint))
        } else {
            Err(anyhow::anyhow!(
                "Could not find non-SOL mint in pair: {:?}",
                mints
            ))
        }
    } else if mints.is_empty() {
        Ok(None)
    } else {
        Err(anyhow::anyhow!(
            "Multiple mints found for instruction: {:?}",
            mints
        ))
    }
}

/// Parses log messages into instruction blocks for a given program.
pub fn get_instruction_blocks(log_messages: &[String], program_id: &str) -> Vec<Vec<String>> {
    let mut blocks = Vec::new();
    let mut current_block = Vec::new();
    let mut invoke_depth = 0;

    for log in log_messages {
        if log.contains(&format!("Program {} invoke", program_id)) {
            invoke_depth += 1;

            if invoke_depth == 1 {
                if !current_block.is_empty() {
                    blocks.push(current_block);
                    current_block = Vec::new();
                }
            }
            current_block.push(log.clone());
        } else if log.contains(&format!("Program {} success", program_id)) {
            current_block.push(log.clone());
            invoke_depth -= 1;

            if invoke_depth == 0 {
                blocks.push(current_block);
                current_block = Vec::new();
            }
        } else if !current_block.is_empty() {
            current_block.push(log.clone());
        }
    }

    if !current_block.is_empty() {
        blocks.push(current_block);
    }

    blocks
}

/// Extracts the instruction type ("Buy", "Sell", "BuyExactQuoteIn", etc.) from a log block.
pub fn extract_instruction_type(block: &[String]) -> Option<String> {
    let mut first_instruction = None;

    for log in block {
        if log.contains("Program log: Instruction: ") {
            let instruction = log
                .trim_start_matches("Program log: Instruction: ")
                .trim()
                .to_string();

            if instruction == "Migrate" {
                return Some(instruction);
            }

            if first_instruction.is_none() {
                first_instruction = Some(instruction);
            }
        } else if log.contains("Program log:") && log.contains("Migrate") {
            return Some("Migrate".to_string());
        }
    }

    first_instruction
}

/// Returns the user (signer) pubkey as a bs58-encoded string.
pub fn get_user(account_keys: &[Vec<u8>]) -> String {
    if account_keys.is_empty() {
        return "unknown".to_string();
    }

    bs58::encode(&account_keys[0]).into_string()
}

/// Extracts pool base/quote reserve amounts from base64-encoded program data.
/// Base reserves at offset 48, quote reserves at offset 56.
pub fn extract_pool_reserves_from_data(base64_data: &str) -> Result<(Option<u64>, Option<u64>)> {
    let bytes = match STANDARD.decode(base64_data) {
        Ok(b) => b,
        Err(_) => return Ok((None, None)),
    };

    if bytes.len() < 64 {
        return Ok((None, None));
    }

    let pool_base_token_reserves = Some(u64::from_le_bytes([
        bytes[48], bytes[49], bytes[50], bytes[51], bytes[52], bytes[53], bytes[54], bytes[55],
    ]));

    let pool_quote_token_reserves = Some(u64::from_le_bytes([
        bytes[56], bytes[57], bytes[58], bytes[59], bytes[60], bytes[61], bytes[62], bytes[63],
    ]));

    Ok((pool_base_token_reserves, pool_quote_token_reserves))
}

/// Extracts token amount from base64-encoded program data at offset 16.
pub fn extract_transaction_amounts(base64_data: &str) -> Result<Option<u64>> {
    let bytes = match STANDARD.decode(base64_data) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };

    if bytes.len() < 24 {
        return Ok(None);
    }

    let token_amount = Some(u64::from_le_bytes([
        bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22], bytes[23],
    ]));

    Ok(token_amount)
}

/// Extracts SOL volume from base64-encoded program data.
/// Returns (buy_volume at offset 112, sell_volume at offset 64).
pub fn extract_sol_volume(base64_data: &str) -> Result<(Option<u64>, Option<u64>)> {
    let bytes = match STANDARD.decode(base64_data) {
        Ok(b) => b,
        Err(_) => return Ok((None, None)),
    };

    if bytes.len() < 120 {
        return Ok((None, None));
    }

    let sell_volume = Some(u64::from_le_bytes([
        bytes[64], bytes[65], bytes[66], bytes[67], bytes[68], bytes[69], bytes[70], bytes[71],
    ]));

    let buy_volume = Some(u64::from_le_bytes([
        bytes[112], bytes[113], bytes[114], bytes[115], bytes[116], bytes[117], bytes[118],
        bytes[119],
    ]));

    Ok((buy_volume, sell_volume))
}

/// Collects all instructions (direct + CPI) for a specific program ID from a transaction.
pub fn get_program_instructions(
    msg: &Message,
    meta: &TransactionStatusMeta,
    full_accounts: &[Vec<u8>],
    program_id: &Pubkey,
) -> Vec<(CompiledInstruction, Option<usize>)> {
    let program_index = full_accounts
        .iter()
        .position(|key| Pubkey::new_from_array(key.clone().try_into().unwrap()) == *program_id);

    let mut all_instrs: Vec<(CompiledInstruction, Option<usize>)> = Vec::new();

    if let Some(program_index) = program_index {
        // top-level
        for instr in msg
            .instructions
            .iter()
            .filter(|i| i.program_id_index as usize == program_index)
        {
            all_instrs.push((instr.clone(), None));
        }
        // inner (CPI)
        for inner in &meta.inner_instructions {
            let parent_idx = inner.index as usize;
            for instr in inner
                .instructions
                .iter()
                .filter(|i| i.program_id_index as usize == program_index)
            {
                let compiled_instr = CompiledInstruction {
                    program_id_index: instr.program_id_index,
                    accounts: instr.accounts.clone(),
                    data: instr.data.clone(),
                };

                all_instrs.push((compiled_instr, Some(parent_idx)));
            }
        }
    }

    all_instrs
}

/// Calculates market cap in SOL (lamports) from pool reserves.
pub fn get_market_cap_in_sol(
    pool_base: u64,
    pool_quote: u64,
    token_amount: u64,
    sol_amount: u64,
    is_buy: bool,
) -> u64 {
    if pool_base == 0 {
        return 0;
    }
    let mut base_real = pool_base as f64 / 1_000_000.0;
    let mut quote_real = pool_quote as f64 / 1_000_000_000.0;

    if is_buy {
        quote_real += sol_amount as f64 / 1_000_000_000.0;
        base_real -= token_amount as f64 / 1_000_000.0;
    } else {
        quote_real -= sol_amount as f64 / 1_000_000_000.0;
        base_real += token_amount as f64 / 1_000_000.0;
    }

    let price_per_token_in_sol = quote_real / base_real;

    let total_supply = 1_000_000_000u64;

    (price_per_token_in_sol * total_supply as f64 * 1_000_000_000.0).round() as u64
}
