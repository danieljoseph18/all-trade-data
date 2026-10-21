pub const PUMP_SWAP_PROGRAM_ID: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
pub const PUMP_PROGRAM_ID: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

pub const SOL_BASE_UNIT: f64 = 1_000_000_000.0;
pub const USDC_BASE_UNIT: f64 = 1_000_000.0;

// === Tip-provider pubkeys ===
// Mirrors the catalog in strategy-data; an inbound transfer to any of these
// addresses during a trade tx is attributed to the named landing service.

pub const TEMPORAL_TIP_ADDRESSES: [&str; 17] = [
    "TEMPaMeCRFAS9EKF53Jd6KpHxgL47uWLcpFArU1Fanq",
    "noz3jAjPiHuBPqiSPkkugaJDkJscPuRhYnSpbi8UvC4",
    "noz3str9KXfpKknefHji8L1mPgimezaiUyCHYMDv1GE",
    "noz6uoYCDijhu1V7cutCpwxNiSovEwLdRHPwmgCGDNo",
    "noz9EPNcT7WH6Sou3sr3GGjHQYVkN3DNirpbvDkv9YJ",
    "nozc5yT15LazbLTFVZzoNZCwjh3yUtW86LoUyqsBu4L",
    "nozFrhfnNGoyqwVuwPAW4aaGqempx4PU6g6D9CJMv7Z",
    "nozievPk7HyK1Rqy1MPJwVQ7qQg2QoJGyP71oeDwbsu",
    "noznbgwYnBLDHu8wcQVCEw6kDrXkPdKkydGJGNXGvL7",
    "nozNVWs5N8mgzuD3qigrCG2UoKxZttxzZ85pvAQVrbP",
    "nozpEGbwx4BcGp6pvEdAh1JoC2CQGZdU6HbNP1v2p6P",
    "nozrhjhkCr3zXT3BiT4WCodYCUFeQvcdUkM7MqhKqge",
    "nozrwQtWhEdrA6W8dkbt9gnUaMs52PdAv5byipnadq3",
    "nozUacTVWub3cL4mJmGCYjKZTnE9RbdY5AP46iQgbPJ",
    "nozWCyTPppJjRuw2fpzDhhWbW355fzosWSzrrMYB1Qk",
    "nozWNju6dY353eMkMqURqwQEoM3SFgEKC6psLCSfUne",
    "nozxNBgWohjR75vdspfxR5H9ceC7XXH99xpxhVGt3Bb",
];

pub const NEXTBLOCK_TIP_ADDRESSES: [&str; 8] = [
    "NextbLoCkVtMGcV47JzewQdvBpLqT9TxQFozQkN98pE",
    "NexTbLoCkWykbLuB1NkjXgFWkX9oAtcoagQegygXXA2",
    "NeXTBLoCKs9F1y5PJS9CKrFNNLU1keHW71rfh7KgA1X",
    "NexTBLockJYZ7QD7p2byrUa6df8ndV2WSd8GkbWqfbb",
    "neXtBLock1LeC67jYd1QdAa32kbVeubsfPNTJC1V5At",
    "nEXTBLockYgngeRmRrjDV31mGSekVPqZoMGhQEZtPVG",
    "NEXTbLoCkB51HpLBLojQfpyVAMorm3zzKg7w9NFdqid",
    "nextBLoCkPMgmG8ZgJtABeScP35qLa2AMCNKntAP7Xc",
];

pub const JITO_TIP_ADDRESSES: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
];

pub const ZEROSLOT_TIP_ADDRESSES: [&str; 22] = [
    "6fQaVhYZA4w3MBSXjJ81Vf6W1EDYeUPXpgVQ6UQyU1Av",
    "4HiwLEP2Bzqj3hM2ENxJuzhcPCdsafwiet3oGkMkuQY4",
    "7toBU3inhmrARGngC7z6SjyP85HgGMmCTEwGNRAcYnEK",
    "8mR3wB1nh4D6J9RUCugxUpc6ya8w38LPxZ3ZjcBhgzws",
    "6SiVU5WEwqfFapRuYCndomztEwDjvS5xgtEof3PLEGm9",
    "TpdxgNJBWZRL8UXF5mrEsyWxDWx9HQexA9P1eTWQ42p",
    "D8f3WkQu6dCF33cZxuAsrKHrGsqGP2yvAHf8mX6RXnwf",
    "GQPFicsy3P3NXxB5piJohoxACqTvWE9fKpLgdsMduoHE",
    "Ey2JEr8hDkgN8qKJGrLf2yFjRhW7rab99HVxwi5rcvJE",
    "4iUgjMT8q2hNZnLuhpqZ1QtiV8deFPy2ajvvjEpKKgsS",
    "3Rz8uD83QsU8wKvZbgWAPvCNDU6Fy8TSZTMcPm3RB6zt",
    "DiTmWENJsHQdawVUUKnUXkconcpW4Jv52TnMWhkncF6t",
    "HRyRhQ86t3H4aAtgvHVpUJmw64BDrb61gRiKcdKUXs5c",
    "7y4whZmw388w1ggjToDLSBLv47drw5SUXcLk6jtmwixd",
    "J9BMEWFbCBEjtQ1fG5Lo9kouX1HfrKQxeUxetwXrifBw",
    "8U1JPQh3mVQ4F5jwRdFTBzvNRQaYFQppHQYoH38DJGSQ",
    "Eb2KpSC8uMt9GmzyAEm5Eb1AAAgTjRaXWFjKyFXHZxF3",
    "FCjUJZ1qozm1e8romw216qyfQMaaWKxWsuySnumVCCNe",
    "ENxTEjSQ1YabmUpXAdCgevnHQ9MHdLv8tzFiuiYJqa13",
    "6rYLG55Q9RpsPGvqdPNJs4z5WTxJVatMB8zV3WJhs5EK",
    "Cix2bHfqPcKcM233mzxbLk14kSggUUiz2A87fJtGivXr",
    "6MgjyQU7G988jgL6EGAgfHYoeesCnwYMyPeh1fpJ71FP",
];

pub const BLOCKROUTE_TIP_ADDRESSES: [&str; 1] = ["HWEoBxYs7ssKuudEjzjmpfJVX7Dvi7wescFsVx2L5yoY"];

pub const NODE_ONE_TIP_ADDRESSES: [&str; 6] = [
    "node1PqAa3BWWzUnTHVbw8NJHC874zn9ngAkXjgWEej",
    "node1UzzTxAAeBTpfZkQPJXBAqixsbdth11ba1NXLBG",
    "node1Qm1bV4fwYnCurP8otJ9s5yrkPq7SPZ5uhj3Tsv",
    "node1PUber6SFmSQgvf2ECmXsHP5o3boRSGhvJyPMX1",
    "node1AyMbeqiVN6eoQzEAwCA6Pk826hrdqdAHR7cdJ3",
    "node1YtWCoTwwVYTFLfS19zquRQzYX332hs1HEuRBjC",
];

pub const ASTRALANE_TIP_ADDRESSES: [&str; 8] = [
    "astrazznxsGUhWShqgNtAdfrzP2G83DzcWVJDxwV9bF",
    "astra4uejePWneqNaJKuFFA8oonqCE1sqF6b45kDMZm",
    "astra9xWY93QyfG6yM8zwsKsRodscjQ2uU2HKNL5prk",
    "astraRVUuTHjpwEVvNBeQEgwYx9w9CFyfxjYoobCZhL",
    "astraEJ2fEj8Xmy6KLG7B3VfbKfsHXhHrNdCQx7iGJK",
    "astraubkDw81n4LuutzSQ8uzHCv4BhPVhfvTcYv8SKC",
    "astraZW5GLFefxNPAatceHhYjfA1ciq9gvfEg2S47xk",
    "astrawVNP4xDBKT7rAdxrLYiTSTdqtUr63fSMduivXK",
];

pub const BLOCKRAZOR_TIP_ADDRESSES: [&str; 14] = [
    "FjmZZrFvhnqqb9ThCuMVnENaM3JGVuGWNyCAxRJcFpg9",
    "6No2i3aawzHsjtThw81iq1EXPJN6rh8eSJCLaYZfKDTG",
    "A9cWowVAiHe9pJfKAj3TJiN9VpbzMUq6E4kEvf5mUT22",
    "Gywj98ophM7GmkDdaWs4isqZnDdFCW7B46TXmKfvyqSm",
    "68Pwb4jS7eZATjDfhmTXgRJjCiZmw1L7Huy4HNpnxJ3o",
    "4ABhJh5rZPjv63RBJBuyWzBK3g9gWMUQdTZP2kiW31V9",
    "B2M4NG5eyZp5SBQrSdtemzk5TqVuaWGQnowGaCBt8GyM",
    "5jA59cXMKQqZAVdtopv8q3yyw9SYfiE3vUCbt7p8MfVf",
    "5YktoWygr1Bp9wiS1xtMtUki1PeYuuzuCF98tqwYxf61",
    "295Avbam4qGShBYK7E9H5Ldew4B3WyJGmgmXfiWdeeyV",
    "EDi4rSy2LZgKJX74mbLTFk4mxoTgT6F7HxxzG2HBAFyK",
    "BnGKHAC386n4Qmv9xtpBVbRaUTKixjBe3oagkPFKtoy6",
    "Dd7K2Fp7AtoN8xCghKDRmyqr5U169t48Tw5fEd3wT9mq",
    "AP6qExwrbRgBAVaehg4b5xHENX815sMabtBzUzVB4v8S",
];

pub const HELIUS_TIP_ADDRESSES: [&str; 10] = [
    "4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCAi87NhDEE",
    "D2L6yPZ2FmmmTKPgzaMKdhu6EWZcTpLy1Vhx8uvZe7NZ",
    "9bnz4RShgq1hAnLnZbP8kbgBg1kEmcJBYQq3gQbmnSta",
    "5VY91ws6B2hMmBFRsXkoAAdsPHBJwRfBht4DXox3xkwn",
    "2nyhqdwKcJZR2vcqCyrYsaPVdAnFoJjiksCXJ7hfEYgD",
    "2q5pghRs6arqVjRvT5gfgWfWcHWmw1ZuCzphgd5KfWGJ",
    "wyvPkWjVZz1M8fHQnMMCDTQDbkManefNNhweYk5WkcF",
    "3KCKozbAaF75qEU33jtzozcJ29yJuaLJTy2jFdzUY8bT",
    "4vieeGHPYPG2MmyPRcYjdiDmmhN3ww7hsFNap8pVN3Ey",
    "4TQLFNWK8AovT1gFvda5jfw2oJeRMKEmw7aH6MGBJ3or",
];

pub const STELLIUM_TIP_ADDRESSES: [&str; 5] = [
    "ste11JV3MLMM7x7EJUM2sXcJC1H7F4jBLnP9a9PG8PH",
    "ste11MWPjXCRfQryCshzi86SGhuXjF4Lv6xMXD2AoSt",
    "ste11p5x8tJ53H1NbNQsRBg1YNRd4GcVpxtDw8PBpmb",
    "ste11p7e2KLYou5bwtt35H7BM6uMdo4pvioGjJXKFcN",
    "ste11TMV68LMi1BguM4RQujtbNCZvf1sjsASpqgAvSX",
];

pub const SOYAS_TIP_ADDRESSES: [&str; 4] = [
    "soyas4s6L8KWZ8rsSk1mF3d1mQScoTGGAgjk98bF8nP",
    "soyascXFW5wEEYiwfEmHy2pNwomqzvggJosGVD6TJdY",
    "soyasDBdKjADwPz3xk82U3TNPRDKEWJj7wWLajNHZ1L",
    "soyasE2abjBAynmHbGWgEwk4ctBy7JMTUCNrMbjcnyH",
];

pub const MOONLAND_TIP_ADDRESSES: [&str; 4] = [
    "moon54DmZ775AepMCPLg1wxGqDNbG3G6CQu3rALggRF",
    "moonwDUA7UjNWiYQFUkymrV21JUw4yEG3rQsTi8kxLq",
    "moonhMCQ4DrQDMwmqZFD34m4t9MsTkeCWqEfx6mZEXn",
    "moonAb4fx2gtqTymPNwSWHmGtTYHJRMz8UJZgWcnMkt",
];

pub const FALCON_TIP_ADDRESSES: [&str; 10] = [
    "Fa1con11xLjPddfzRwRUB16sbFZggp2JeJkCeWREyR8X",
    "Fa1con113Bvi76nS5AzUiRDC2fqjfzkNMUNRLgQybMYt",
    "Fa1con1zUzb6qJVFz5tNkPq1Ahm8H1qKW7Q48252QbkQ",
    "Fa1con1i7mpa7Qc6epYJ6r4P9AbU77DFFz173r59Df1x",
    "Fa1con1GKusK2EqsfzrDzGPaYZSxQtFGzJiRMMU9Zm2g",
    "Fa1con11TM1RuAQzbQzYjTy4Ekfap9Lnc9fnEbQYEd6Q",
    "Fa1con1QGHJK232s8yZpzZZwqPexnAKcoyKj626LNsMv",
    "Fa1con16d3MSwd3SAiwvr2LwgkpE7ot8zntbpuec8HAx",
    "Fa1con18nWn8TdAGL7JX8PertfMUGVSc899NawokJ4Bq",
    "Fa1con1RDwVwM9VrJ53CwVefD3VU9c58EMpDawV7fLMi",
];

/// Position of base_mint within a PumpSwap buy/sell/buy_exact_quote_in instruction's accounts.
pub const PUMP_SWAP_MINT_IX_POS: usize = 3;
/// Position of the pool within a PumpSwap trade instruction's accounts.
pub const PUMP_SWAP_POOL_IX_POS: usize = 0;
/// Position of quote_mint within a PumpSwap buy/sell/buy_exact_quote_in instruction's
/// accounts. Pump AMM allows arbitrary quote mints; this collector accepts SOL
/// and USDC pools and tags USDC rows with `is_usdc`.
pub const PUMP_SWAP_QUOTE_MINT_IX_POS: usize = 4;

// === PumpSwap instruction discriminators (first 8 bytes of instruction data) ===
pub const AMM_BUY_DISCRIMINATOR: [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
pub const AMM_SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];
pub const BUY_EXACT_IN_DISCRIMINATOR: [u8; 8] = [198, 46, 21, 82, 180, 217, 232, 112];
pub const BOOST_BUY_AND_BURN_DISCRIMINATOR: [u8; 8] = [105, 68, 6, 175, 0, 7, 35, 162];

// v2 trades (pump-public-docs 8cda1fa, 2026-10-08; live on mainnet since the
// 2026-10-02 deploy). Same account prefix as v1 (pool 0, user 1, base_mint 3,
// quote_mint 4) and the same leading two u64 args; they emit the same
// BuyEvent / SellEvent. The bytes equal pump's own `buy_v2` / `sell_v2` /
// `buy_exact_quote_in_v2`, since an Anchor disc hashes only the ix name —
// always match them together with the PumpSwap program id.
pub const AMM_BUY_V2_DISCRIMINATOR: [u8; 8] = [184, 23, 238, 97, 103, 197, 211, 61];
pub const AMM_SELL_V2_DISCRIMINATOR: [u8; 8] = [93, 246, 130, 60, 231, 233, 64, 178];
pub const AMM_BUY_EXACT_QUOTE_IN_V2_DISCRIMINATOR: [u8; 8] = [194, 171, 28, 70, 104, 77, 91, 47];
/// `multi_hop_swap(amount_in, min_amount_out)`: one ix trading a route of 1+
/// PumpSwap pools and/or pump bonding curves. Pool hops emit their own
/// BuyEvent / SellEvent as direct children; curve hops run through pump's
/// `multi_hop_curve_swap` and are not recorded here.
pub const AMM_MULTI_HOP_SWAP_DISCRIMINATOR: [u8; 8] = [43, 100, 73, 19, 233, 246, 111, 148];
/// `multi_hop_swap` fixed accounts (user 0, user_in_token_account 1, ...);
/// hop groups follow.
pub const PUMP_SWAP_MULTI_HOP_FIXED_ACCOUNTS: usize = 16;
/// Per hop: `[base_mint, quote_mint, pool | bonding_curve, base_vault, quote_vault]`.
pub const PUMP_SWAP_MULTI_HOP_ACCOUNTS_PER_HOP: usize = 5;
/// `multi_hop_swap` user (fee payer of the route).
pub const PUMP_SWAP_MULTI_HOP_USER_IX_POS: usize = 0;
/// `multi_hop_swap` user_in_token_account: the account the route spends from.
pub const PUMP_SWAP_MULTI_HOP_USER_IN_IX_POS: usize = 1;

// === PumpSwap Anchor self-CPI event discriminators ===
/// BuyEvent — emitted for buy / buy_exact_quote_in
pub const PUMP_SWAP_BUY_EVENT_DISC: [u8; 8] = [103, 244, 82, 31, 44, 245, 119, 119];
/// SellEvent
pub const PUMP_SWAP_SELL_EVENT_DISC: [u8; 8] = [62, 47, 55, 10, 165, 3, 220, 42];
/// BoostBuyAndBurnEvent — protocol-funded buy that changes pool price/reserves.
pub const PUMP_SWAP_BOOST_BUY_EVENT_DISC: [u8; 8] = [63, 69, 28, 22, 48, 92, 194, 185];

// === Raydium CPMM (stonk.fun post-migration venue) ===
//
// stonk.fun launches on Raydium LaunchLab (bonding curve) and migrates into
// Raydium CPMM. This collector records every post-migration CPMM swap on a
// whitelisted stonk.fun mint into `alt_trades`. Addresses, discriminators and
// account positions mirror strategy-data (`STONK_FUN_PLAN.md` §1.1 / §1.6)
// and are verified against the fixture in `fixtures/stonk/`.

/// Raydium CPMM (constant-product AMM) program.
pub const RAYDIUM_CPMM_PROGRAM_ID: &str = "CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C";
/// CPMM `amm_config` (index 11) that every stonk.fun migration pool is created
/// under (`PlatformConfig.cpswap_config`). Swaps under any other config are
/// manually created pools for the same mint and are never the migration pool.
pub const STONK_CPMM_AMM_CONFIG: &str = "CRRS5ieQmBrZjWhcj99JuGrT5tyuWDaGAXLXLFjbAtjQ";
/// Platform tag written to `alt_trades.platform`.
pub const STONK_PLATFORM_NAME: &str = "stonk_fun";
/// Venue tag written to `alt_trades.venue`.
pub const RAYDIUM_CPMM_VENUE_NAME: &str = "raydium_cpmm";

// Raydium CPMM instruction discriminators (raydium_cp_swap IDL v0.2.0).
/// `swap_base_input(amount_in, minimum_amount_out)`.
pub const CPMM_SWAP_BASE_INPUT_DISCRIMINATOR: [u8; 8] = [143, 190, 90, 218, 196, 30, 51, 222];
/// `swap_base_output(max_amount_in, amount_out)`.
pub const CPMM_SWAP_BASE_OUTPUT_DISCRIMINATOR: [u8; 8] = [55, 217, 98, 86, 163, 74, 180, 173];
/// Anchor `SwapEvent` discriminator. Shared by every Anchor program with an
/// event named `SwapEvent` (e.g. Raydium CLMM, 213-byte body); the parser
/// guards on the exact 162-byte CPMM body length.
pub const CPMM_SWAP_EVENT_DISC: [u8; 8] = [64, 198, 205, 232, 38, 8, 113, 226];

// CPMM swap_base_input / swap_base_output account positions (13).
pub const CPMM_SWAP_PAYER_IX_POS: usize = 0;
pub const CPMM_SWAP_AMM_CONFIG_IX_POS: usize = 2;
pub const CPMM_SWAP_POOL_STATE_IX_POS: usize = 3;
pub const CPMM_SWAP_INPUT_VAULT_IX_POS: usize = 6;
pub const CPMM_SWAP_OUTPUT_VAULT_IX_POS: usize = 7;
pub const CPMM_SWAP_INPUT_MINT_IX_POS: usize = 10;
pub const CPMM_SWAP_OUTPUT_MINT_IX_POS: usize = 11;
pub const CPMM_SWAP_MIN_ACCOUNTS: usize = 13;

/// Seed prefix of the CPMM `PoolState` PDA:
/// `PDA("pool", amm_config, token_0_mint, token_1_mint)` with the two mints
/// in ascending pubkey-byte order.
pub const CPMM_POOL_SEED: &[u8] = b"pool";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::pump_swap_trade_ix;
    use serde_json::Value;

    /// pump-fun/pump-public-docs `idl/pump_amm.json` @ 8cda1fa (2026-10-08).
    const PUMP_AMM_IDL: &str = include_str!("../../fixtures/pump/pump_amm_idl.json");

    fn idl_ix<'a>(idl: &'a Value, name: &str) -> &'a Value {
        idl["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ix| ix["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing from the IDL"))
    }

    fn disc(v: &Value) -> [u8; 8] {
        let bytes: Vec<u8> = v["discriminator"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b.as_u64().unwrap() as u8)
            .collect();
        bytes.try_into().unwrap()
    }

    fn account_pos(ix: &Value, name: &str) -> usize {
        ix["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .position(|a| a["name"] == name)
            .unwrap_or_else(|| panic!("{} has no account {name}", ix["name"]))
    }

    /// Every discriminator and account position the handler relies on, read
    /// from the vendored IDL — so a future IDL refresh fails `cargo test`
    /// instead of silently dropping trades in production.
    #[test]
    fn pump_swap_discriminators_and_positions_match_idl() {
        let idl: Value = serde_json::from_str(PUMP_AMM_IDL).unwrap();

        let trade_ixs = [
            ("buy", AMM_BUY_DISCRIMINATOR),
            ("buy_exact_quote_in", BUY_EXACT_IN_DISCRIMINATOR),
            ("sell", AMM_SELL_DISCRIMINATOR),
            ("buy_v2", AMM_BUY_V2_DISCRIMINATOR),
            (
                "buy_exact_quote_in_v2",
                AMM_BUY_EXACT_QUOTE_IN_V2_DISCRIMINATOR,
            ),
            ("sell_v2", AMM_SELL_V2_DISCRIMINATOR),
            ("boost_buy_and_burn", BOOST_BUY_AND_BURN_DISCRIMINATOR),
        ];
        for (name, constant) in trade_ixs {
            let ix = idl_ix(&idl, name);
            assert_eq!(disc(ix), constant, "{name} discriminator");
            let kind =
                pump_swap_trade_ix(&constant).unwrap_or_else(|| panic!("{name} unclassified"));
            assert_eq!(kind.name, name);
            if name == "boost_buy_and_burn" {
                continue;
            }
            assert_eq!(
                account_pos(ix, "pool"),
                PUMP_SWAP_POOL_IX_POS,
                "{name} pool"
            );
            assert_eq!(account_pos(ix, "user"), 1, "{name} user");
            assert_eq!(
                account_pos(ix, "base_mint"),
                PUMP_SWAP_MINT_IX_POS,
                "{name} base_mint"
            );
            assert_eq!(
                account_pos(ix, "quote_mint"),
                PUMP_SWAP_QUOTE_MINT_IX_POS,
                "{name} quote_mint"
            );
            // The two leading u64 args the failed-tx path reads, in order.
            let args: Vec<(&str, &str)> = ix["args"]
                .as_array()
                .unwrap()
                .iter()
                .take(2)
                .map(|a| {
                    (
                        a["name"].as_str().unwrap(),
                        a["type"].as_str().unwrap_or(""),
                    )
                })
                .collect();
            let quote_first = args[0].0.contains("quote");
            assert_eq!(quote_first, kind.is_exact_in, "{name} arg order {args:?}");
            assert!(
                args.iter().all(|(_, t)| *t == "u64"),
                "{name} args {args:?}"
            );
        }

        // Every buy/sell-family instruction in the IDL must be classified (or
        // be multi_hop_swap), so a new version can't slip in unnoticed.
        for ix in idl["instructions"].as_array().unwrap() {
            let name = ix["name"].as_str().unwrap();
            if (name.starts_with("buy") || name.starts_with("sell")) && name != "buyback" {
                assert!(
                    pump_swap_trade_ix(&disc(ix)).is_some(),
                    "PumpSwap trade instruction {name} is not handled"
                );
            }
        }

        let mh = idl_ix(&idl, "multi_hop_swap");
        assert_eq!(disc(mh), AMM_MULTI_HOP_SWAP_DISCRIMINATOR);
        assert_eq!(
            mh["accounts"].as_array().unwrap().len(),
            PUMP_SWAP_MULTI_HOP_FIXED_ACCOUNTS
        );
        assert_eq!(account_pos(mh, "user"), PUMP_SWAP_MULTI_HOP_USER_IX_POS);
        assert_eq!(
            account_pos(mh, "user_in_token_account"),
            PUMP_SWAP_MULTI_HOP_USER_IN_IX_POS
        );
        let mh_args: Vec<&str> = mh["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap())
            .collect();
        assert_eq!(mh_args, ["amount_in", "min_amount_out"]);

        let events = idl["events"].as_array().unwrap();
        for (name, constant) in [
            ("BuyEvent", PUMP_SWAP_BUY_EVENT_DISC),
            ("SellEvent", PUMP_SWAP_SELL_EVENT_DISC),
            ("BoostBuyAndBurnEvent", PUMP_SWAP_BOOST_BUY_EVENT_DISC),
        ] {
            let ev = events.iter().find(|e| e["name"] == name).unwrap();
            assert_eq!(disc(ev), constant, "{name} discriminator");
        }
    }
}
