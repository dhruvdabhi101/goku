import csv
import statistics
import sys


with open(sys.argv[1], newline="", encoding="utf-8-sig") as source:
    rows = list(csv.DictReader(source))

groups = {}
for row in rows:
    if int(row["errors"]) != 0 or int(row["frames"]) != 3 or int(row["jpeg_bytes"]) <= 0:
        raise SystemExit("benchmark did not complete the expected work")
    key = (int(row["width"]), int(row["height"]), row["variant"])
    groups.setdefault(key, []).append(row)

print("size,variant,runs,median_ms_per_frame,min_ms,max_ms,jpeg_share_percent")
for (width, height, variant), samples in sorted(groups.items()):
    if len(samples) < 5:
        raise SystemExit("at least five samples per variant are required")
    times = [float(row["total_ms"]) / int(row["frames"]) for row in samples]
    jpeg_share = statistics.median(float(row["jpeg_ms"]) / float(row["total_ms"]) * 100 for row in samples)
    print(f"{width}x{height},{variant},{len(samples)},{statistics.median(times):.3f},{min(times):.3f},{max(times):.3f},{jpeg_share:.1f}")
