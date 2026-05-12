pub const PUMP_SWAP_PROGRAM_ID: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

/// Position of base_mint within a PumpSwap buy/sell/buy_exact_quote_in instruction's accounts.
pub const PUMP_SWAP_MINT_IX_POS: usize = 3;
/// Position of quote_mint within a PumpSwap buy/sell/buy_exact_quote_in instruction's
/// accounts. Used to reject non-SOL-paired pools — pump_amm allows arbitrary
/// quote mints, but every downstream amount in this collector (sol_amount,
/// market_cap) assumes lamports.
pub const PUMP_SWAP_QUOTE_MINT_IX_POS: usize = 4;

// === PumpSwap instruction discriminators (first 8 bytes of instruction data) ===
pub const AMM_BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
pub const AMM_SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];
pub const BUY_EXACT_IN_DISCRIMINATOR: [u8; 8] = [198, 46, 21, 82, 180, 217, 232, 112];

// === PumpSwap Anchor self-CPI event discriminators ===
/// BuyEvent — emitted for buy / buy_exact_quote_in
pub const PUMP_SWAP_BUY_EVENT_DISC: [u8; 8] = [103, 244, 82, 31, 44, 245, 119, 119];
/// SellEvent
pub const PUMP_SWAP_SELL_EVENT_DISC: [u8; 8] = [62, 47, 55, 10, 165, 3, 220, 42];
