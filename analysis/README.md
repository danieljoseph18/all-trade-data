# Validator behavior analysis — corrected full-block study

No database rows or objects were changed. Every SQL transaction is explicitly read-only.

## Population

- 76,676 leader-attributed slots across 731 observed leaders.
- 7,075,868 unique trades from the full blocks containing those slots. The KOL sample table is used only to map slot to leader; analysis is not restricted to the KOL transaction, wallet, or mint.
- 10,824,871 of the system's named-lane sends matched to the broad stream for same-race provider comparisons.
- The broad stream's insertion order was calibrated against exact transaction indexes on 227,654 overlapping rows across 18,982 slots. Direction agreed for 99.2634% of 208,672 adjacent observations. Only 14.5999% were truly consecutive block indexes, so insertion distance is not treated as an exact microsecond or micro-block measurement.

## Ordering cohorts

The strongest cohort groups cross-signer transactions in the same slot, mint, and non-null `compute_account`. These are responses to the same target and are the closest available approximation to near-simultaneous competitors. The analysis also requires corroboration from the much larger same-mint, cross-signer population.

A validator enters `high_confidence_validator_evidence.csv` only when it has at least 100 close response-burst comparisons across at least 50 event-balanced slots, at least 5,000 corroborating same-mint comparisons, at least three qualifying weeks, a close-burst rate of at least 82%, and no qualifying week below 75%. Conflicting/stale client metadata is excluded.

This proves a stable on-chain association between priority fee and earlier order. It does not prove causation because packet arrival timestamps are unavailable.

## Provider experiment

Named lanes are compared inside the identical `(slot, mint, leader)` race. Repeated copies are collapsed to the earliest on-chain copy for each lane. Pairwise results shown in `high_confidence_provider_routes.csv` require fees within 5%, at least 300 matched comparisons, separation from the runner-up, and the same winner in both observed weeks.

The provider report covers third-party lanes only. It does not compare Jito bundles or direct TPU, and its period is limited to 2026-07-07 through 2026-07-14. Results measure the configured route as operated, including its fanout behavior—not intrinsic provider latency.

## Critical tip limitation

The database tip registry has 169 addresses for Jito and named send providers, but none of Rakurai's eight native tip accounts. Consequently, the local data cannot directly test Rakurai MREV ordering. A generic sum of all detected transfer tips is invalid because only scheduler-recognized destinations matter.

Official Rakurai documentation states that its scheduler uses recognized tip plus fees relative to estimated compute cost, and explicitly says a high tip does not replace a regular priority fee. Jito documentation distinguishes regular transactions (both priority fee and Jito tip; it recommends a 70/30 split) from bundles (Jito tip auction). BAM documents approximately 50 ms fee-density batches. Harmonic documentation states that its bundle tips are native priority fees, while validator scheduling strategy is configurable.

## Files

- `export_leader_blocks.sql`: full-block read-only export.
- `analyze_whole_blocks.py`: event-balanced fee/tip ordering analysis.
- `whole_block_ordering.csv`: complete validator/cohort/gap/week/load metrics.
- `export_lane_order.sql`: named-lane to on-chain-order export.
- `analyze_lane_order.py`: same-race route comparison.
- `lane_order_summary.csv` and `lane_order_pairs.csv`: full provider evidence.
- `high_confidence_validator_evidence.csv`: strict empirical subset only.
- `high_confidence_provider_routes.csv`: strict provider subset only.
