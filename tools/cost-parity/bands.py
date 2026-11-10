"""The work clock's error by component on the parity bench's rows (src/cost.rs BENCH_BANDS): each row's ree / client against its
fastest kept session (reference/), rows in ns, by fit and held-out rows, and each component's held-out 10th to 90th percentile.

    python -I tools/cost-parity/bands.py <ree side json>... [--priced] [--names-from=<ree side json>]

--priced leaves out the rows whose work counts something priced at nothing (a large buffer's fresh memory, a buffer call's bytes
past 64 KB: cost.rs LUAU_BUFFER_FRESH_BYTE), as BENCH_BANDS does; --names-from takes those rows from another side (one made by a
ree that does not count them)."""
import statistics
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import parity  # noqa: E402

# Which component of the work clock each family of rows tests
COMPONENT = {
    "lang": "luau", "native": "luau", "lib": "luau", "libsize": "luau", "loop": "luau",
    "read": "engine", "write": "engine", "method": "engine", "find": "engine", "attr": "engine", "create": "engine",
    "clone": "engine", "parent": "engine", "destroy": "engine", "tween": "engine", "datatype": "engine", "json": "engine",
    "signal": "dispatch", "task": "dispatch",
    "gc": "collector",
}
COMPONENTS = ("luau", "engine", "dispatch", "collector")

# The counters of work the clock counts and prices at nothing
UNPRICED_COUNTERS = ("buffreshbytes", "bufbigcopybytes")

# The kept client sessions
CLIENT_RUNS = []
for session_file in sorted((HERE / "reference").glob("*.json")):
    CLIENT_RUNS.extend(parity.load(session_file))


def counts_unpriced_work(row):
    """Whether the row's work counts bytes the clock prices at nothing"""
    work = row.get("work") or {}
    return any((work.get(counter) or 0) > 0 for counter in UNPRICED_COUNTERS)


def compared_rows(ree_path):
    """The ree side's rows compared with each row's fastest client session"""
    ree_run = parity.load(ree_path)[0]
    return parity.compare(ree_run, CLIENT_RUNS, fastest=True)


def unpriced_row_names(ree_path):
    return {row["name"] for row in compared_rows(ree_path) if counts_unpriced_work(row)}


def percentiles(ratios):
    """The 10th and 90th percentile of the sorted `ratios`; a short list reads its least and greatest"""
    if len(ratios) >= 10:
        deciles = statistics.quantiles(ratios, n=10)
    else:
        deciles = [ratios[0]] * 9 + [ratios[-1]]
    return deciles[0], deciles[-1]


def median_error_percent(ratios):
    return statistics.median(abs(ratio - 1) for ratio in ratios) * 100


def print_fit_and_held_out(rows):
    for side in ("fit", "held"):
        held_out = side == "held"
        errors = [abs(row["ratio"] - 1) for row in rows if row["heldOut"] == held_out]
        within_quarter = sum(error <= 0.25 for error in errors)
        within_half = sum(error <= 0.5 for error in errors)
        print(f"  {side:5s} n={len(errors):3d} median |err| {statistics.median(errors) * 100:5.1f} %  within 25%: {within_quarter:3d}  within 50%: {within_half:3d}")


def print_components(rows):
    for component in COMPONENTS:
        ratios = sorted(row["ratio"] for row in rows if row["heldOut"] and COMPONENT.get(row["family"]) == component)
        low, high = percentiles(ratios)
        print(f"  held {component:9s} n={len(ratios):3d} ratio p10 {low:5.2f} median {statistics.median(ratios):5.2f} p90 {high:5.2f}  median |err| {median_error_percent(ratios):5.1f} %")
    held_ratios = sorted(row["ratio"] for row in rows if row["heldOut"])
    deciles = statistics.quantiles(held_ratios, n=10)
    print(f"  held all n={len(held_ratios)} p10 {deciles[0]:.2f} p90 {deciles[-1]:.2f} median |err| {median_error_percent(held_ratios):.1f} %")


def print_lowest_luau_rows(rows, count=6):
    luau_rows = [row for row in rows if row["heldOut"] and COMPONENT.get(row["family"]) == "luau"]
    for row in sorted(luau_rows, key=lambda row: row["ratio"])[:count]:
        print(f"    low luau: {row['name']} {row['ratio']:.2f}")


def print_families(rows):
    errors_by_family = {}
    for row in rows:
        errors_by_family.setdefault(row["family"], []).append(abs(row["ratio"] - 1))
    families = ", ".join(f"{family} {statistics.median(errors) * 100:.0f} ({len(errors)})" for family, errors in sorted(errors_by_family.items()))
    print("  by family median |err| %:", families)


def bands(ree_path, priced_only, names_from):
    rows = [row for row in compared_rows(ree_path) if row["unit"] == "ns" and row["ratio"]]
    if names_from:
        # rows another ree counted unpriced work in count it here too
        names = unpriced_row_names(names_from)
        for row in rows:
            if row["name"] in names:
                row["work"] = dict(row.get("work") or {}, buffreshbytes=1)
    left_out = sorted(row["name"] for row in rows if counts_unpriced_work(row))
    if priced_only:
        rows = [row for row in rows if not counts_unpriced_work(row)]
    with_or_without = "without" if priced_only else "with"
    print(f"== {Path(ree_path).name}: {len(rows)} rows ({with_or_without} {len(left_out)} rows that count unpriced work: {', '.join(left_out)})")
    print_fit_and_held_out(rows)
    print_components(rows)
    print_lowest_luau_rows(rows)
    print_families(rows)
    return rows


def main():
    paths = [arg for arg in sys.argv[1:] if not arg.startswith("--")]
    priced_only = "--priced" in sys.argv
    names_from = None
    for arg in sys.argv[1:]:
        if arg.startswith("--names-from="):
            names_from = arg.split("=", 1)[1]
    for path in paths:
        bands(path, priced_only, names_from)


if __name__ == "__main__":
    main()
