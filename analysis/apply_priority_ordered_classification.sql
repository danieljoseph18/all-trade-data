-- Operational priority-order classification.
--
-- TRUE means priority-fee ordering is expected to be materially relevant for
-- transaction construction. Confidence in that prediction is represented
-- separately by policy_confidence; this boolean is not an evidence threshold.
BEGIN;
SET LOCAL lock_timeout = '10s';
SET LOCAL statement_timeout = '30s';

DO $$
DECLARE
  expected_true integer;
BEGIN
  SELECT count(*)
  INTO expected_true
  FROM public.validator_profiles
  WHERE priority_fee_importance IN ('material', 'dominant');

  IF expected_true <> 662 THEN
    RAISE EXCEPTION 'Classification precondition failed: expected 662 TRUE rows, found %', expected_true;
  END IF;
END $$;

UPDATE public.validator_profiles
SET is_priority_ordered = priority_fee_importance IN ('material', 'dominant')
WHERE is_priority_ordered IS DISTINCT FROM
      (priority_fee_importance IN ('material', 'dominant'));

COMMIT;
