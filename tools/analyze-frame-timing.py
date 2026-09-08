#!/usr/bin/env python3
"""Summarize stable pacing windows. Never average per-window percentiles."""
import argparse
import collections
import json

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("path")
parser.add_argument("--output", default="eDP-1")
parser.add_argument("--phase")
args = parser.parse_args()

groups = {}
skipped = collections.Counter()
with open(args.path, encoding="utf-8") as source:
    for line in source:
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            skipped["incomplete_json"] += 1
            continue
        if row.get("type") != "window" or row.get("output") != args.output:
            continue
        if args.phase and row.get("phase") != args.phase:
            continue
        if row.get("phase") == "startup" and args.phase != "startup":
            skipped["startup"] += 1
            continue
        counters = row.get("counters", {})
        if not row.get("recording", True):
            skipped["recording_off"] += 1
            continue
        if row.get("dropped_windows", 0) or counters.get("StreamRegistrations", 0):
            skipped["transition_or_loss"] += 1
            continue
        if row.get("elapsed_ns", 0) < 4_000_000_000:
            skipped["partial_window"] += 1
            continue
        key = (row.get("pid"), row.get("epoch"), row.get("phase", "unlabeled"), row.get("direct_target"))
        group = groups.setdefault(key, {"elapsed": 0, "windows": 0, "counters": collections.Counter(), "metrics": {}})
        group["elapsed"] += row["elapsed_ns"]
        group["windows"] += 1
        group["counters"].update(counters)
        for name, metric in row.get("metrics", {}).items():
            if not metric.get("count"):
                continue
            dst = group["metrics"].setdefault(name, {"count": 0, "total": 0, "max": 0, "min": None, "worst_p99": 0})
            dst["count"] += metric["count"]
            dst["total"] += metric["total_ns"]
            dst["max"] = max(dst["max"], metric["max_ns"])
            dst["worst_p99"] = max(dst["worst_p99"], metric["p99_ns"])
            if "min_ns" in metric:
                dst["min"] = metric["min_ns"] if dst["min"] is None else min(dst["min"], metric["min_ns"])

for (pid, epoch, phase, direct), group in groups.items():
    seconds = group["elapsed"] / 1e9
    counters = group["counters"]
    print(f"\n{args.output}: pid={pid} epoch={epoch} phase={phase} direct={direct}, {group['windows']} windows / {seconds:.2f}s")
    print(f"submitted={counters['FramesSubmitted']/seconds:.3f}/s presented={counters['PresentEvents']/seconds:.3f}/s "
          f"sequence_gaps={counters['SequenceGaps']} late_presentations={counters['PresentLate']} "
          f"queue_start_late={counters['QueuePastDeadline']} queue_return_late={counters['QueueReturnedPastDeadline']}")
    print("counters:", dict(sorted(counters.items())))
    print(f"{'stage':28s} {'calls':>7s} {'mean ms':>10s} {'max ms':>10s} {'worst p99*':>11s} {'min ms':>10s}")
    for name, metric in group["metrics"].items():
        mean = metric["total"] / metric["count"] / 1e6
        minimum = '-' if metric["min"] is None else f"{metric['min']/1e6:.4f}"
        print(f"{name:28s} {metric['count']:7d} {mean:10.4f} {metric['max']/1e6:10.4f} {metric['worst_p99']/1e6:11.4f} {minimum:>10s}")
print("\n* Worst single-window p99 upper bound, NOT a merged percentile. Nested stage durations overlap.")
print("Sequence gaps can be idle/client gaps, not necessarily missed compositor deadlines.")
print("Skipped windows:", dict(skipped))
