-- Export every observed trade from every slot whose leader is known through
-- validator_profile_samples. This uses the sample table only as a slot-to-leader
-- map; it does not filter to KOL wallets or KOL mints.
BEGIN READ ONLY;
SET LOCAL statement_timeout = '30min';

COPY (
  WITH leader_slots AS MATERIALIZED (
    SELECT DISTINCT slot
    FROM pre_migration_data.validator_profile_samples
  )
  SELECT b.block_number AS slot, b.id, b.signature, b.signer,
         b.transaction_type, b.mint_address,
         b.transaction_timestamp, b.fee_sol, b.tips, b.tip_company,
         b.is_failed, b.compute_account, b.cu_price_micro_lamports,
         b.cu_limit, b.cu_consumed, b.quote_kind
  FROM pre_migration_data.buy_sell_data b
  WHERE EXISTS (SELECT 1 FROM leader_slots ls WHERE ls.slot=b.block_number)
) TO STDOUT WITH (FORMAT CSV, HEADER true);

COMMIT;
