-- Match the system's named send lanes to the broad stream ordering proxy.
BEGIN READ ONLY;
SET LOCAL statement_timeout = '30min';
COPY (
  SELECT o.slot,o.mint,o.lane,o.lane_name,o.leader_identity,o.leader_class,
         o.success,o.fee_lamports,o.cu_price,o.observed_at,b.id,
         b.tips,b.tip_company,b.compute_account
  FROM public.opspam_lands o
  JOIN pre_migration_data.buy_sell_data b ON b.signature=o.signature
  WHERE o.kind='buy'
) TO STDOUT WITH (FORMAT CSV, HEADER true);
COMMIT;
