#!/usr/bin/env python3
"""Head-to-head lane ordering inside identical (slot, mint, leader) races."""
import csv
import sys
from collections import defaultdict
from datetime import datetime


def main(inp, summary_out, pair_out):
    # race -> lane -> [earliest_id, lane_name, copies, fee, cu_price]
    races = {}
    weeks = {}
    with open(inp, newline="") as f:
        for r in csv.DictReader(f):
            key = (int(r["slot"]), r["mint"], r["leader_identity"], r["leader_class"])
            lane = int(r["lane"])
            rid = int(r["id"])
            d = races.setdefault(key, {})
            if key not in weeks:
                dt = datetime.strptime(r["observed_at"][:10], "%Y-%m-%d").isocalendar()
                weeks[key] = f"{dt[0]}-W{dt[1]:02d}"
            cur = d.get(lane)
            if cur is None:
                d[lane] = [rid, r["lane_name"], 1, int(r["fee_lamports"] or 0), int(r["cu_price"] or 0)]
            else:
                cur[2] += 1
                if rid < cur[0]:
                    cur[0], cur[1], cur[3], cur[4] = rid, r["lane_name"], int(r["fee_lamports"] or 0), int(r["cu_price"] or 0)

    # [presence, firsts, neutral_expected, copies, rank_sum]
    stats = defaultdict(lambda: [0, 0, 0.0, 0, 0.0])
    # [comparisons, wins_a, matched_fee_comparisons, matched_fee_wins_a]
    pairs = defaultdict(lambda: [0, 0, 0, 0])
    analyzable = 0
    for key, lanes in races.items():
        if len(lanes) < 2:
            continue
        analyzable += 1
        leader, klass = key[2], key[3]
        week = weeks[key]
        ordered = sorted(lanes.items(), key=lambda x: x[1][0])
        n = len(ordered)
        winner = ordered[0][0]
        ranks = {lane: i for i, (lane, _) in enumerate(ordered)}
        for lane, v in lanes.items():
            for period in ("all", f"week:{week}"):
                a = stats[(leader, klass, lane, v[1], period)]
                a[0] += 1; a[1] += lane == winner; a[2] += 1 / n; a[3] += v[2]; a[4] += ranks[lane] / (n - 1)
        lane_ids = sorted(lanes)
        for i, a_lane in enumerate(lane_ids):
            for b_lane in lane_ids[i+1:]:
                av, bv = lanes[a_lane], lanes[b_lane]
                out = pairs[(leader, klass, a_lane, av[1], b_lane, bv[1])]
                out[0] += 1
                out[1] += av[0] < bv[0]
                af, bf = av[3], bv[3]
                if min(af, bf) > 0 and max(af, bf) / min(af, bf) <= 1.05:
                    out[2] += 1
                    out[3] += av[0] < bv[0]

    with open(summary_out, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["identity","leader_class","lane","lane_name","period","race_presence","firsts","neutral_expected_firsts","first_rate","lift_vs_neutral","mean_copies","mean_normalized_rank","analyzable_races_total"])
        for k, a in sorted(stats.items()):
            presence, firsts, expected, copies, rank_sum = a
            w.writerow([*k,presence,firsts,expected,firsts/presence,(firsts/expected if expected else ""),copies/presence,rank_sum/presence,analyzable])
    with open(pair_out, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["identity","leader_class","lane_a","name_a","lane_b","name_b","comparisons","a_wins","a_win_rate","matched_fee_comparisons","matched_fee_a_wins","matched_fee_a_win_rate"])
        for k, a in sorted(pairs.items()):
            w.writerow([*k,a[0],a[1],a[1]/a[0],a[2],a[3],a[3]/a[2] if a[2] else ""])


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("usage: analyze_lane_order.py INPUT SUMMARY_OUT PAIR_OUT")
    main(*sys.argv[1:])
