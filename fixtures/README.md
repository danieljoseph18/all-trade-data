# Test fixtures

`stonk/tx_cpmm_swap_base_input_2cY4Pc3j.json` — mainnet Raydium CPMM
`swap_base_input` (stonk.fun post-migration sell of
`HrSjY8Wz…` against DOGE, slot 446404878), captured 2026-09-12 with the
`json` encoding. Copied from `strategy-data/fixtures/stonk/`; the numbers it
pins are documented in that repo's `STONK_FUN_PLAN.md` §7. Used by
`utils::cpmm` and `handler::cpmm_handler` tests.

`pump/upgrade_2026_10/` — 18 real transactions of the 2026-10-08 Pump /
PumpSwap upgrade (PumpSwap v2 trades, `multi_hop_swap` routes of every shape,
synthetic migrations and the `migrate_v2` pools they open, pump v3 trades),
listed with network/signature/slot in `manifest.json`. Copied from
`strategy-data/fixtures/pump/upgrade_2026_10/`. Used by
`handler::pump_upgrade_replay_tests`.

`pump/pump_amm_idl.json` — pump-fun/pump-public-docs `idl/pump_amm.json` @
8cda1fa. `utils::constants` tests read every discriminator and account
position the AMM handler uses from it.
