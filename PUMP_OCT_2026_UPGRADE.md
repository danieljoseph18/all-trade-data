# Pump / PumpSwap upgrade (2026-10-08) in all-trade-data

Source: pump-fun/pump-public-docs @ `8cda1fa`. Same release handled in
strategy-data (`strategy-data/PUMP_OCT_2026_UPGRADE_PLAN.md`); this collector
only records PumpSwap pool trades (`amm_trades`), so only the PumpSwap side
applies.

## Impact before this change

| Upgrade change | Effect on this collector | Fix |
|---|---|---|
| PumpSwap `buy_v2` / `sell_v2` / `buy_exact_quote_in_v2` (live on mainnet since the 2026-10-02 deploy) | **Silently dropped**: dispatch listed discriminators explicitly. `amm_trades` had no v2 rows at all; ~15% of successful pool trades were v2 on 2026-10-09. | `pump_swap_trade_ix` classifier (one list for every trade disc). Same accounts, args and events as v1, so everything downstream is shared. |
| `multi_hop_swap` (live on mainnet by 2026-10-09) | Dropped. One ix trades several pools; each pool hop emits its own BuyEvent/SellEvent. | Event-driven: every Buy/SellEvent the ix emits itself is matched to its hop group by the event's `pool`. Same pool filters as single trades. |
| Multi-hop middle coin (A in SOL → A → B) | n/a | Recorded (it is a real pool trade that moves price) as `instruction_type = multi_hop_swap_routed`; `user_pubkey` is the route's user, who never holds A. The end coin is `multi_hop_swap`. |
| Failed multi-hop routes | n/a | Recorded like failed single trades: each hop's side from the route's input mint, `amount_in` on the first hop's input side, `min_amount_out` on the last hop's output side, `amount_source = instruction_request`. |
| Pools quoted in anything but SOL/USDC (stock tokens, PUMP, pump coins) | Filtered (quote not SOL/USDC) | Recorded since 2026-10-12 (see below). In real routes the user's end coin trades on a pump-coin-quoted pool. |
| Synthetic migration (`PostCompleteBuyEvent`) | Not a PumpSwap trade: emitted by pump before the pool exists, and a v3 curve buy is not in this stream (filter = PumpSwap/CPMM). | Not recorded, deliberately: only 6 of 40,754 whitelisted mints were whitelisted before migrating. `migrate_v2` creates the same canonical pool (tested), so all later pool trades are recorded. |
| Pump v3 curve trades | Not in scope (curve trades are not recorded). | None. |
| Appended event fields, negative `virtual_quote_reserves`, v1 transactions | Already safe (front-walked decoders, signed effective reserves). | `analysis/audit_pump_amm_coverage.py` now requests `maxSupportedTransactionVersion: 1`. |

## Code

- `utils/constants.rs`: v2 / `multi_hop_swap` discs and hop layout; IDL test.
- `utils/transaction.rs`: `pump_swap_trade_ix`, `multi_hop_swap_hops`,
  `multi_hop_is_routed`, `multi_hop_input_mint`, `multi_hop_sides`,
  `extract_pump_swap_event_pool`, `direct_child_events` (`find_event_data`
  is built on it), `canonical_pump_swap_pool`.
- `handler/amm_handler.rs`: tx-level context + one `emit` path; single
  trades via the classifier; `multi_hop_fills`.
- `analysis/audit_pump_amm_coverage.py`: same instructions, decoded
  independently (routed = payer's balance of that coin didn't move).

## Verification

- `cargo test` (46): IDL-frozen discs/positions/arg order (and every
  buy*/sell* ix in the IDL must be classified); synthetic v1+v2 variants
  direct and via CPI, failed and successful; one-hop routes; unknown-pool
  event; nested-trade event not claimed by its route; real-fixture replay
  against truth decoded from raw RPC JSON; failed-route replay;
  `migrate_v2` pool = canonical PDA.
- Mutation checks: dropping a v2 disc, the multi-hop branch, the routed
  rule, the event-depth bound, or the failed-route side/amount logic each
  fails the suite.
- Live mainnet shadow, 2026-10-09, 15 min (slots 454926827–454930891,
  1,166,415 txs; scratch harness, not committed): new handler, old handler
  (HEAD) and an independent chain decoder on every tx, whitelist bypassed.
  - Successful pool trades vs chain truth: every Buy/SellEvent on a
    canonical SOL/USDC pool recorded once with the right pool, mint, user,
    side, amount and kind. One disagreement: a one-hop route whose output
    went to another wallet's account, which the truth's "payer balance
    didn't move" heuristic misreads as routed; the handler is right.
  - v1-only txs: 646,166 successful and all failed txs byte-identical to
    the old handler (amounts, market cap, flags).
  - v2: 12,799 successful + 14,970 failed records the old handler drops;
    `multi_hop_swap` live on mainnet: 354 pool hops (351 routed).
  - Against production `amm_trades` for the same slots (whitelisted mints):
    every v1 row identical once production's whitelist refresh caught up;
    production lacked 26,226 v2/multi-hop rows in those 15 minutes.

## Any quote mint (2026-10-12)

`amm_trades` now records canonical PumpSwap pools of whitelisted mints with
any quote mint, not just SOL/USDC: stock tokens (xStocks, SPCX...), PUMP,
and pump coins (the end coin of a multi-hop route). About 1,000 whitelisted
mints have such a quote.

- `QuoteCurrency::Other(mint)`; the eligibility filter keeps a non-WSOL,
  whitelisted base on its canonical pool, whatever the quote.
- `sol_amount` and `market_cap` are in the quote's own atomic units, as
  for USDC. `quote_mint_address` names the unit; `is_usdc` stays false.
  Market cap in native units does not depend on quote decimals.
- **Consumers must filter `quote_mint_address`** before treating
  `sol_amount` / `market_cap` as lamports (sell-sim `etl.py`/`prepare.py`
  and the big-dipper dashboard queries do not today; USDC rows had the
  same issue already).
- Tests: `records_canonical_pools_with_any_quote_mint`; the real-tx replay
  now expects 10 records (4 end-coin hops on pump-coin-quoted pools) and 8
  failed-route hops.
- Deployed 2026-10-12 10:20 UTC (resumed from slot 456011558). First
  minutes: 78 other-quote rows across PUMP, SPCX, xStocks and pump-coin
  quotes, every successful one with a market cap; no warnings.
