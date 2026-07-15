#!/usr/bin/env python3
"""Analyze ordering in all trades from leader-attributed blocks.

Input is produced by export_leader_blocks.sql. The script keeps the database out
of the analytical loop and writes only local CSV summaries.
"""
from __future__ import annotations

import csv
import math
import sys
from collections import defaultdict
from datetime import datetime


class Agg:
    __slots__ = ("pairs", "cmp", "wins", "slot_rates", "tip_prio_conflicts", "tip_first")

    def __init__(self):
        self.pairs = 0
        self.cmp = [0] * 5
        self.wins = [0] * 5
        self.slot_rates = [[] for _ in range(5)]
        self.tip_prio_conflicts = 0
        self.tip_first = 0

    def merge_slot(self, other: "Agg"):
        self.pairs += other.pairs
        self.tip_prio_conflicts += other.tip_prio_conflicts
        self.tip_first += other.tip_first
        for i in range(5):
            self.cmp[i] += other.cmp[i]
            self.wins[i] += other.wins[i]
            if other.cmp[i]:
                self.slot_rates[i].append(other.wins[i] / other.cmp[i])


def intern(s: str) -> str:
    return sys.intern(s) if s else ""


def integer(s: str) -> int:
    return int(s) if s else 0


def tip_lamports(s: str) -> int:
    return round(float(s) * 1_000_000_000) if s else 0


def gap_bucket(gap: int) -> str:
    if gap <= 4:
        return "01-04"
    if gap <= 16:
        return "05-16"
    if gap <= 64:
        return "17-64"
    if gap <= 256:
        return "65-256"
    return "257+"


def load_bucket(n: int) -> str:
    return "low" if n < 25 else "medium" if n < 100 else "high"


def add_pair(a, b, agg: Agg):
    # tuple: id,mint,signer,compute,cu_price,cu_limit,cu_consumed,tip,week,tip_company,type
    ap = a[4] * a[5] / 1_000_000
    bp = b[4] * b[5] / 1_000_000
    at, bt = a[7], b[7]
    ac = a[6] or a[5] or 1
    bc = b[6] or b[5] or 1
    values_a = (a[4], ap, at, ap + at, (ap + at) / ac)
    values_b = (b[4], bp, bt, bp + bt, (bp + bt) / bc)
    agg.pairs += 1
    for i, (x, y) in enumerate(zip(values_a, values_b)):
        if x != y:
            agg.cmp[i] += 1
            agg.wins[i] += x > y
    prio_dir = (ap > bp) - (ap < bp)
    tip_dir = (at > bt) - (at < bt)
    if prio_dir and tip_dir and prio_dir != tip_dir:
        agg.tip_prio_conflicts += 1
        agg.tip_first += tip_dir > 0


def mean_se(xs):
    n = len(xs)
    if not n:
        return "", "", 0
    m = sum(xs) / n
    if n == 1:
        return m, "", n
    var = sum((x - m) ** 2 for x in xs) / (n - 1)
    return m, math.sqrt(var / n), n


def main(block_csv: str, slot_csv: str, output_csv: str):
    slot_meta = {}
    with open(slot_csv, newline="") as f:
        for r in csv.DictReader(f):
            slot_meta[int(r["slot"])] = (intern(r["leader_identity"]), intern(r["software_client"]), r["name"])

    # Strings are interned; compact tuples keep ~7m rows tractable in memory.
    slots = defaultdict(list)
    with open(block_csv, newline="") as f:
        for r in csv.DictReader(f):
            slot = int(r["slot"])
            dt = datetime.strptime(r["transaction_timestamp"][:10], "%Y-%m-%d")
            iso = dt.isocalendar()
            week = f"{iso[0]}-W{iso[1]:02d}"
            slots[slot].append((
                int(r["id"]), intern(r["mint_address"]), intern(r["signer"]),
                intern(r["compute_account"]), integer(r["cu_price_micro_lamports"]),
                integer(r["cu_limit"]), integer(r["cu_consumed"]), tip_lamports(r["tips"]),
                week, intern(r["tip_company"]), intern(r["transaction_type"]),
            ))

    global_aggs = defaultdict(Agg)
    provider_counts = defaultdict(lambda: [0, 0])
    slot_counts = defaultdict(int)
    tx_counts = defaultdict(int)

    for slot, txs in slots.items():
        meta = slot_meta.get(slot)
        if not meta:
            continue
        leader, client, _ = meta
        txs.sort(key=lambda x: x[0])
        slot_counts[leader] += 1
        tx_counts[leader] += len(txs)
        week = txs[0][8]
        load = load_bucket(len(txs))
        local = defaultdict(Agg)

        for t in txs:
            provider_counts[(leader, t[9] or "(none)")][0] += 1
            provider_counts[(leader, t[9] or "(none)")][1] += t[7] > 0

        by_mint = defaultdict(list)
        by_compute = defaultdict(list)
        for t in txs:
            by_mint[t[1]].append(t)
            if t[3]:
                by_compute[(t[1], t[3])].append(t)

        def process(seq, cohort):
            for a, b in zip(seq, seq[1:]):
                gap = gap_bucket(b[0] - a[0])
                if cohort.endswith("cross_signer") and a[2] == b[2]:
                    continue
                if cohort == "mint_same_signer" and a[2] != b[2]:
                    continue
                for period in ("all", f"week:{week}", f"load:{load}"):
                    add_pair(a, b, local[(cohort, gap, period)])

        for seq in by_mint.values():
            if len(seq) > 1:
                process(seq, "mint_all")
                process(seq, "mint_cross_signer")
                process(seq, "mint_same_signer")
        for seq in by_compute.values():
            if len(seq) > 1:
                process(seq, "compute_cross_signer")

        for (cohort, gap, period), a in local.items():
            global_aggs[(leader, client, cohort, gap, period)].merge_slot(a)

    names = ("cu_price", "priority_fee", "transfer_tip", "total_revenue", "revenue_per_consumed_cu")
    fields = ["identity", "software_client", "cohort", "gap", "period", "slots_total", "txs_total", "pairs",
              "tip_priority_conflicts", "tip_first_rate"]
    for n in names:
        fields += [f"{n}_comparisons", f"{n}_pooled_rate", f"{n}_slot_mean", f"{n}_slot_se", f"{n}_slots"]
    with open(output_csv, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        for (leader, client, cohort, gap, period), a in sorted(global_aggs.items()):
            row = {"identity": leader, "software_client": client, "cohort": cohort, "gap": gap,
                   "period": period, "slots_total": slot_counts[leader], "txs_total": tx_counts[leader],
                   "pairs": a.pairs, "tip_priority_conflicts": a.tip_prio_conflicts,
                   "tip_first_rate": a.tip_first / a.tip_prio_conflicts if a.tip_prio_conflicts else ""}
            for i, n in enumerate(names):
                mean, se, nslots = mean_se(a.slot_rates[i])
                row[f"{n}_comparisons"] = a.cmp[i]
                row[f"{n}_pooled_rate"] = a.wins[i] / a.cmp[i] if a.cmp[i] else ""
                row[f"{n}_slot_mean"] = mean
                row[f"{n}_slot_se"] = se
                row[f"{n}_slots"] = nslots
            w.writerow(row)

    with open(output_csv.replace(".csv", "_tip_companies.csv"), "w", newline="") as f:
        w = csv.writer(f); w.writerow(["identity", "tip_company", "transactions", "tipped_transactions"])
        for (leader, company), v in sorted(provider_counts.items()):
            w.writerow([leader, company, *v])


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("usage: analyze_whole_blocks.py BLOCKS.csv SLOT_MAP.csv OUTPUT.csv")
    main(*sys.argv[1:])
