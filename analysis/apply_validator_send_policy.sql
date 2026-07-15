-- Adds and populates cache-friendly validator send policies.
-- Existing columns are intentionally never assigned by this migration.
BEGIN;
SET LOCAL lock_timeout = '10s';
SET LOCAL statement_timeout = '2min';

ALTER TABLE public.validator_profiles
  ADD COLUMN IF NOT EXISTS send_policy jsonb,
  ADD COLUMN IF NOT EXISTS policy_version bigint NOT NULL DEFAULT 0,
  ADD COLUMN IF NOT EXISTS policy_confidence text NOT NULL DEFAULT 'none',
  ADD COLUMN IF NOT EXISTS policy_source text,
  ADD COLUMN IF NOT EXISTS profiled_at timestamptz,
  ADD COLUMN IF NOT EXISTS policy_valid_until timestamptz,
  ADD COLUMN IF NOT EXISTS behavior_class text NOT NULL DEFAULT 'unknown',
  ADD COLUMN IF NOT EXISTS priority_fee_importance text NOT NULL DEFAULT 'unknown',
  ADD COLUMN IF NOT EXISTS tip_policy text NOT NULL DEFAULT 'unknown',
  ADD COLUMN IF NOT EXISTS preferred_submission_mode text,
  ADD COLUMN IF NOT EXISTS fallback_provider text;

WITH classified AS (
  SELECT p.identity,
    CASE
      WHEN p.software_client IN ('AgaveBam','FireBam') THEN 'bam_fee_density'
      WHEN p.software_client = 'Rakurai' THEN 'rakurai_mrev'
      WHEN p.software_client = 'JitoLabs' THEN 'jito_hybrid'
      WHEN p.software_client LIKE 'Harmonic%' THEN 'harmonic_builder'
      WHEN p.software_client = 'Agave' THEN 'standard_priority'
      WHEN p.software_client IN ('Firedancer','Frankendancer') THEN 'configurable_scheduler'
      ELSE 'unknown'
    END AS behavior_class,
    CASE
      WHEN p.identity IN (
        '9jxgosAfHgHzwnxsHw4RAZYaLVokMbnYtmiZBreynGFP',
        '9rkJMARqK6VBkcxGfKBAwnA44gPAfGxPbPsfsggFNDSQ',
        'DRpbCBMxVnDK7maPM5tGv6MvB3v1sRMC86PZ8okm21hy',
        'EvnRmnMrd69kFdbLMxWkTn1icZ7DCceRhvmb2SJXqDo4',
        'Fd7btgySsrjuo25CJCj7oE7VPMyezDhnx7pZkj2v69Nk',
        'HEL1USMZKAL2odpNBj2oCjffnFGaYwmbGmyewGv1e2TU',
        'JD549HsbJHeEKKUrKgg4Fj2iyv2RGjsV7NTZjZUrHybB',
        'q9XWcZ7T1wP4bW9SB4XgNNwjnFEJ982nE8aVbbNuwot'
      ) THEN 'high'
      WHEN p.software_client IN ('AgaveBam','FireBam','Rakurai','JitoLabs')
        OR p.software_client LIKE 'Harmonic%' THEN 'medium'
      WHEN p.software_client IN ('Agave','Firedancer','Frankendancer') THEN 'low'
      ELSE 'none'
    END AS confidence,
    CASE
      WHEN p.identity IN (
        '9jxgosAfHgHzwnxsHw4RAZYaLVokMbnYtmiZBreynGFP',
        '9rkJMARqK6VBkcxGfKBAwnA44gPAfGxPbPsfsggFNDSQ',
        'DRpbCBMxVnDK7maPM5tGv6MvB3v1sRMC86PZ8okm21hy',
        'EvnRmnMrd69kFdbLMxWkTn1icZ7DCceRhvmb2SJXqDo4',
        'Fd7btgySsrjuo25CJCj7oE7VPMyezDhnx7pZkj2v69Nk',
        'HEL1USMZKAL2odpNBj2oCjffnFGaYwmbGmyewGv1e2TU',
        'JD549HsbJHeEKKUrKgg4Fj2iyv2RGjsV7NTZjZUrHybB',
        'q9XWcZ7T1wP4bW9SB4XgNNwjnFEJ982nE8aVbbNuwot'
      ) THEN 'whole_block_observation+client_documentation'
      WHEN p.software_client IN ('AgaveBam','FireBam','Rakurai','JitoLabs')
        OR p.software_client LIKE 'Harmonic%' THEN 'client_documentation'
      WHEN p.software_client IN ('Agave','Firedancer','Frankendancer') THEN 'client_default'
      ELSE 'unknown'
    END AS source,
    CASE
      WHEN p.software_client IN ('AgaveBam','FireBam') THEN 'dominant'
      WHEN p.software_client IN ('Rakurai','JitoLabs','Agave') OR p.software_client LIKE 'Harmonic%' THEN 'material'
      WHEN p.identity IN (
        '9jxgosAfHgHzwnxsHw4RAZYaLVokMbnYtmiZBreynGFP',
        'EvnRmnMrd69kFdbLMxWkTn1icZ7DCceRhvmb2SJXqDo4',
        'JD549HsbJHeEKKUrKgg4Fj2iyv2RGjsV7NTZjZUrHybB'
      ) THEN 'material'
      ELSE 'unknown'
    END AS priority_importance,
    CASE
      WHEN p.software_client = 'Rakurai' THEN 'rakurai_native'
      WHEN p.software_client = 'JitoLabs' THEN 'jito_tip'
      WHEN p.software_client LIKE 'Harmonic%' THEN 'priority_fee_as_tip'
      -- This top-level policy is for ordinary transactions. Jito tipping for
      -- direct-TPU clients is represented only in the bundle policy below.
      WHEN p.software_client IN ('AgaveBam','FireBam','Agave','Firedancer','Frankendancer') THEN 'none'
      ELSE 'none'
    END AS tip_policy,
    CASE
      WHEN p.software_client IN ('AgaveBam','FireBam','Agave','Firedancer','Frankendancer') THEN 'direct_tpu'
      WHEN p.software_client = 'JitoLabs' THEN 'jito_transaction'
      WHEN p.software_client IN ('Rakurai') OR p.software_client LIKE 'Harmonic%' THEN 'provider'
      ELSE 'multi_route'
    END AS preferred_mode,
    CASE p.identity
      WHEN '3psxMyr7rQzywVp1MXKd1XFmFz33NjydzCoJx9t2sMQW' THEN 'Helius'
      WHEN '4mtXJ5pUcMMB4t8cLbi7zfDJCHfYLRrQb4qSLmh57sKL' THEN 'AstraLane'
      WHEN 'aXiomFkk6VzXaBhPuhMqTLZZguCFzzbyP9LTtZ7ZHLQ' THEN 'landX'
      WHEN 'Fd7btgySsrjuo25CJCj7oE7VPMyezDhnx7pZkj2v69Nk' THEN 'landX'
      WHEN 'Ha1iade1AH3B12K9SccfWoPdFtQKKQsj2ZyWwxcjqJJU' THEN 'stellium'
      WHEN 'q9XWcZ7T1wP4bW9SB4XgNNwjnFEJ982nE8aVbbNuwot' THEN 'landX'
      ELSE NULL
    END AS fallback_provider,
    CASE p.identity
      WHEN '3psxMyr7rQzywVp1MXKd1XFmFz33NjydzCoJx9t2sMQW' THEN 'Temporal'
      WHEN '4mtXJ5pUcMMB4t8cLbi7zfDJCHfYLRrQb4qSLmh57sKL' THEN 'Temporal'
      WHEN 'aXiomFkk6VzXaBhPuhMqTLZZguCFzzbyP9LTtZ7ZHLQ' THEN 'Temporal'
      WHEN 'Fd7btgySsrjuo25CJCj7oE7VPMyezDhnx7pZkj2v69Nk' THEN 'NodeOne'
      WHEN 'Ha1iade1AH3B12K9SccfWoPdFtQKKQsj2ZyWwxcjqJJU' THEN 'Temporal'
      WHEN 'q9XWcZ7T1wP4bW9SB4XgNNwjnFEJ982nE8aVbbNuwot' THEN 'Temporal'
      ELSE NULL
    END AS observed_route
  FROM public.validator_profiles p
), payload AS (
  SELECT p.identity,p.runs_jito,p.best_provider,
    c.behavior_class,c.confidence,c.source,c.priority_importance,c.tip_policy,
    c.preferred_mode,c.fallback_provider,c.observed_route,
    CASE c.behavior_class
      WHEN 'bam_fee_density' THEN 50
      ELSE NULL
    END AS documented_window_ms,
    CASE
      WHEN c.tip_policy IN ('rakurai_native','jito_tip') THEN 'material'
      WHEN c.tip_policy = 'priority_fee_as_tip' THEN 'native_priority_fee'
      WHEN c.tip_policy = 'none' THEN 'none'
      ELSE 'secondary'
    END AS tip_importance
  FROM public.validator_profiles p JOIN classified c USING(identity)
)
UPDATE public.validator_profiles p
SET policy_version = 1,
    policy_confidence = x.confidence,
    policy_source = x.source,
    profiled_at = timestamptz '2026-07-15 08:33:32+00',
    policy_valid_until = now() + CASE WHEN x.confidence='high' THEN interval '7 days' ELSE interval '3 days' END,
    behavior_class = x.behavior_class,
    priority_fee_importance = x.priority_importance,
    tip_policy = x.tip_policy,
    preferred_submission_mode = x.preferred_mode,
    fallback_provider = x.fallback_provider,
    send_policy = jsonb_build_object(
      'schema_version', 1,
      'status', CASE x.confidence WHEN 'high' THEN 'validator_verified' WHEN 'none' THEN 'unknown' ELSE 'client_default' END,
      'behavior', jsonb_build_object(
        'class', x.behavior_class,
        'confidence', x.confidence,
        'basis', x.source,
        'priority_fee_importance', x.priority_importance,
        'tip_importance', x.tip_importance,
        'ordering_window_ms', x.documented_window_ms
      ),
      'regular_transaction', jsonb_build_object(
        'submission_modes', CASE x.behavior_class
          WHEN 'bam_fee_density' THEN jsonb_build_array('direct_tpu','provider')
          WHEN 'rakurai_mrev' THEN jsonb_build_array('provider','direct_tpu')
          WHEN 'jito_hybrid' THEN jsonb_build_array('jito_transaction','direct_tpu','provider')
          WHEN 'harmonic_builder' THEN jsonb_build_array('provider','direct_tpu')
          ELSE jsonb_build_array('direct_tpu','provider') END,
        'priority_fee', jsonb_build_object('mode','dynamic','importance',x.priority_importance),
        'tip', jsonb_build_object('mode',x.tip_policy,'importance',x.tip_importance)
      ),
      'bundle', jsonb_build_object(
        'supported', (x.runs_jito OR x.behavior_class IN ('rakurai_mrev','harmonic_builder','jito_hybrid','bam_fee_density')),
        'submission_modes', CASE
          WHEN x.behavior_class='harmonic_builder' THEN jsonb_build_array('harmonic_bundle')
          WHEN x.behavior_class='rakurai_mrev' THEN jsonb_build_array('rakurai_bundle')
          WHEN x.runs_jito OR x.behavior_class IN ('jito_hybrid','bam_fee_density') THEN jsonb_build_array('jito_bundle')
          ELSE '[]'::jsonb END,
        'priority_fee', CASE
          WHEN x.behavior_class='harmonic_builder' THEN jsonb_build_object('mode','dynamic','importance','dominant','acts_as_tip',true)
          WHEN x.behavior_class='rakurai_mrev' THEN jsonb_build_object('mode','dynamic','importance','material')
          ELSE jsonb_build_object('mode','optional','importance','secondary') END,
        'tip', CASE
          WHEN x.behavior_class='harmonic_builder' THEN jsonb_build_object('mode','priority_fee_as_tip','importance','dominant')
          WHEN x.behavior_class='rakurai_mrev' THEN jsonb_build_object('mode','rakurai_native','importance','material')
          WHEN x.runs_jito OR x.behavior_class IN ('jito_hybrid','bam_fee_density') THEN jsonb_build_object('mode','jito_tip','importance','dominant')
          ELSE jsonb_build_object('mode','none','importance','none') END
      ),
      'routes', CASE
        WHEN x.observed_route IS NOT NULL AND lower(coalesce(x.best_provider,''))=lower(x.observed_route)
          THEN jsonb_build_array(jsonb_build_object('provider',x.observed_route,'rank',1,'scope','third_party_regular','confidence','high','observed_from','2026-07-07','observed_to','2026-07-14'))
        WHEN x.observed_route IS NOT NULL
          THEN (CASE WHEN x.best_provider IS NULL THEN '[]'::jsonb ELSE jsonb_build_array(jsonb_build_object('provider',x.best_provider,'rank',1,'scope','existing_default','confidence','unverified')) END)
             || jsonb_build_array(jsonb_build_object('provider',x.observed_route,'rank',1,'scope','third_party_regular','confidence','high','observed_from','2026-07-07','observed_to','2026-07-14'))
        WHEN x.best_provider IS NOT NULL
          THEN jsonb_build_array(jsonb_build_object('provider',x.best_provider,'rank',1,'scope','existing_default','confidence','unverified'))
        ELSE '[]'::jsonb END,
      'fallback', jsonb_build_object(
        'priority_fee','conservative',
        'submission_modes',jsonb_build_array('direct_tpu','provider'),
        'provider',x.fallback_provider
      ),
      'evidence', jsonb_build_object(
        'profiled_at','2026-07-15T08:33:32Z',
        'validator_specific',(x.confidence='high'),
        'provider_scope',CASE WHEN x.observed_route IS NULL THEN NULL ELSE 'third_party_lanes' END
      )
    )
FROM payload x
WHERE p.identity=x.identity;

ALTER TABLE public.validator_profiles
  ALTER COLUMN send_policy SET NOT NULL,
  ALTER COLUMN send_policy SET DEFAULT '{"schema_version":1,"status":"unknown","behavior":{"class":"unknown","confidence":"none","priority_fee_importance":"unknown","tip_importance":"unknown","ordering_window_ms":null},"regular_transaction":{"submission_modes":["direct_tpu","provider"],"priority_fee":{"mode":"dynamic","importance":"unknown"},"tip":{"mode":"unknown","importance":"unknown"}},"bundle":{"supported":false,"submission_modes":[]},"routes":[],"fallback":{"priority_fee":"conservative","submission_modes":["direct_tpu","provider"],"provider":null}}'::jsonb;

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname='validator_profiles_policy_confidence_check' AND conrelid='public.validator_profiles'::regclass) THEN
    ALTER TABLE public.validator_profiles ADD CONSTRAINT validator_profiles_policy_confidence_check
      CHECK (policy_confidence IN ('none','low','medium','high'));
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname='validator_profiles_priority_fee_importance_check' AND conrelid='public.validator_profiles'::regclass) THEN
    ALTER TABLE public.validator_profiles ADD CONSTRAINT validator_profiles_priority_fee_importance_check
      CHECK (priority_fee_importance IN ('unknown','minimal','secondary','material','dominant'));
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname='validator_profiles_tip_policy_check' AND conrelid='public.validator_profiles'::regclass) THEN
    ALTER TABLE public.validator_profiles ADD CONSTRAINT validator_profiles_tip_policy_check
      CHECK (tip_policy IN ('unknown','none','jito_tip','rakurai_native','priority_fee_as_tip','custom'));
  END IF;
END$$;

COMMIT;
