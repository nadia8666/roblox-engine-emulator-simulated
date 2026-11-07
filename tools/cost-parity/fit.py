"""Prices from the parity bench: what each engine call and each kind of Luau work should cost on ree's work clock.

    python -I tools/cost-parity/fit.py residuals ree.json engine.json [engine2.json ...] [--family write]
    python -I tools/cost-parity/fit.py luau ree.json engine.json [...] [--native] [--free key,key]
    python -I tools/cost-parity/fit.py library ree.json engine.json [...]

`ree.json` is `parity.py ree` output: every row carries the VM's counts of one round (REE_WORK_COUNTERS=1). The Luau part of
a row is those counts at src/cost.rs's Luau prices (mirrored below; keep them equal); the rest of the engine's time is what the
engine calls of the row cost, so `residuals` prints, per row, the engine call's price that would make the row exact beside what
ree charges now. `luau` fits the price of each kind of instruction (CLASSES) on the `lang` rows, interpreted, or the `native`
rows with --native (and with --free, those prices too), and `library` the library's prices (LIBRARY) on the `lib` rows: non-negative
least squares on relative error, using only rows that are not held out (parity.held_out); each prints every row's error with the
fitted prices, the held-out ones apart. Needs numpy and scipy for `luau` and `library`.
"""
import argparse
import re
import statistics
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import parity  # noqa: E402

ROOT = HERE.parents[1]
_SOURCE = (ROOT / "vendor/luau0-src/luau/Common/include/Luau/Bytecode.h").read_text()
_ENUM = _SOURCE[_SOURCE.index("enum LuauOpcode"):]
OPS = re.findall(r"^\s*LOP_([A-Z0-9_]+),", _ENUM[:_ENUM.index("};")], re.M)

# Opcode classes, priced apart (src/cost.rs INTERPRETED_OPS and NATIVE_OPS): what a code generator turns into a few machine
# instructions apart from what keeps a call or a table walk
CLASSES = {
    "c_simple": {"NOP", "BREAK", "LOADNIL", "LOADB", "LOADN", "LOADK", "LOADKX", "MOVE", "GETUPVAL", "SETUPVAL", "JUMP", "JUMPBACK", "JUMPX",
                 "JUMPIF", "JUMPIFNOT", "JUMPIFEQ", "JUMPIFLE", "JUMPIFLT", "JUMPIFNOTEQ", "JUMPIFNOTLE", "JUMPIFNOTLT", "JUMPXEQKNIL",
                 "JUMPXEQKB", "JUMPXEQKN", "JUMPXEQKS", "ADD", "SUB", "MUL", "DIV", "ADDK", "SUBK", "MULK", "DIVK", "SUBRK", "DIVRK", "AND",
                 "OR", "ANDK", "ORK", "NOT", "MINUS", "FORNPREP", "FORNLOOP", "COVERAGE", "CLOSEUPVALS", "PREPVARARGS", "CAPTURE",
                 "CMPPROTO", "NEWCLASSMEMBER", "FINCONSTRUCT"},
    "c_mod": {"MOD", "MODK", "POW", "POWK", "IDIV", "IDIVK"},
    "c_tablek": {"GETTABLEKS", "SETTABLEKS", "GETTABLEN", "SETTABLEN", "LENGTH", "SETLIST", "GETUDATAKS", "SETUDATAKS", "GETPRIVATEFIELD",
                 "SETPRIVATEFIELD"},
    "c_tabledyn": {"GETTABLE", "SETTABLE"},
    "c_alloc": {"NEWTABLE", "DUPTABLE", "NEWCLOSURE", "DUPCLOSURE", "CONCAT", "GETVARARGS", "NEWCLASS", "CONSTRUCT"},
    "c_call": {"CALL", "RETURN", "NATIVECALL", "NAMECALLUDATA", "CALLFB", "FASTPCALL"},
    "c_iter": {"FORGLOOP", "FORGPREP", "FORGPREP_INEXT", "FORGPREP_NEXT"},
    "c_fastcall": {"FASTCALL", "FASTCALL1", "FASTCALL2", "FASTCALL2K", "FASTCALL3"},
    "c_global": {"GETGLOBAL", "SETGLOBAL", "GETIMPORT"},
    # a method call's lookup: Luau 0.746's interpreter dispatches the CALL after it apart (0.694 counted the two as one NAMECALL)
    "c_namecall": {"NAMECALL"},
}

# src/cost.rs's Luau prices (nanoseconds): an instruction by its class, interpreted (INTERPRETED_OPS) and in a native module
# (NATIVE_OPS), and the rest of `luau`'s counts (with the vector members' charges)
INTERPRETED = {"c_simple": 0.48, "c_mod": 5.46, "c_tablek": 2.45, "c_tabledyn": 2.20, "c_alloc": 0.0, "c_call": 4.89, "c_iter": 2.54,
               "c_fastcall": 3.75, "c_global": 1.36, "c_namecall": 0.0}
NATIVE = {"c_simple": 0.43, "c_mod": 0.65, "c_tablek": 0.42, "c_tabledyn": 0.75, "c_alloc": 1.74, "c_call": 3.25, "c_iter": 1.93,
          "c_fastcall": 0.19, "c_global": 0.27, "c_namecall": 3.25}
LIBRARY = {"numtostr": 18.0, "newstring": 171.0, "sortcompare": 1.06, "sortcall": 9.7, "createitem": 4.3, "cloneitem": 4.3,
           "concatitem": 7.1, "splitpart": 25.4, "format": 102.0, "formatspec": 0.0}
LUAU = {"object": 22.0, "stored": 0.4, "rehash": 12.0, "ccall": 5.0, "hostcall": 15.0, "magnitude": 12.5, "unit": 9.1,
        "vectormethod": 0.0, "slowimport": 7.9, **LIBRARY}


def counts(work, key="ops"):
    """{opcode name: count} of a row's work counts"""
    return {OPS[int(op)]: n for op, n in (work.get(key) or {}).items()}


def classes(work, n, key="ops"):
    """A row's instructions per operation of the row by class (CLASSES), interpreted (`ops`) or native (`nativeops`)"""
    table = counts(work, key)
    return {name: sum(count for op, count in table.items() if op in ops) / n for name, ops in CLASSES.items()}


def features(work, n):
    """The Luau model's counts of a row other than its instructions, per operation of the row."""
    return {
        "numtostr": work.get("numtostr", 0) / n,
        "sortcompare": work.get("sortcompares", 0) / n,
        "sortcall": work.get("sortcalls", 0) / n,
        "createitem": work.get("createitems", 0) / n,
        "cloneitem": work.get("cloneitems", 0) / n,
        "concatitem": work.get("concatitems", 0) / n,
        "splitpart": work.get("splitparts", 0) / n,
        "format": work.get("formats", 0) / n,
        "formatspec": work.get("formatspecs", 0) / n,
        "newstring": work.get("newstrings", 0) / n,
        "slowimport": work.get("slowimports", 0) / n,
        "object": work.get("objects", 0) / n,
        "stored": work.get("stored", 0) / n,
        "rehash": work.get("rehashed", 0) / n,
        "ccall": work.get("ccalls", 0) / n,
        "hostcall": work.get("hostcalls", 0) / n,
        "hostnanos": work.get("hostnanos", 0) / n,
        "magnitude": work.get("vectormagnitudes", 0) / n,
        "unit": work.get("vectorunits", 0) / n,
        "vectormethod": work.get("vectormethods", 0) / n,
    }


def luau_part(work, n, interpreted=INTERPRETED, native=NATIVE, prices=LUAU):
    """A row's Luau work at these prices, nanoseconds per operation of the row"""
    f = features(work, n)
    ci, cn = classes(work, n, "ops"), classes(work, n, "nativeops")
    return (sum(prices[k] * f[k] for k in prices) + f["hostnanos"] + sum(interpreted[c] * ci[c] for c in interpreted)
            + sum(native[c] * cn[c] for c in native))


def rows_of(path_ree, paths_engine, family=None):
    ree = parity.load(path_ree)[0]
    engines = [run for path in paths_engine for run in parity.load(path)]
    rows = parity.compare(ree, engines, family)
    n_of = {row["name"]: row["n"] for row in ree["rows"]}
    for row in rows:
        row["n"] = n_of[row["name"]]
    return rows


def residuals(args):
    rows = rows_of(args.ree, args.engine, args.family)
    print(f"{'workload':50s} {'engine':>9s} {'ree':>9s} {'luau':>8s} {'charge':>9s} {'target':>9s} {'x':>6s}")
    for row in sorted(rows, key=lambda r: r["name"]):
        if not row.get("work") or row["unit"] != "ns":
            continue
        part = luau_part(row["work"], row["n"])
        charge = row["ree"] - part
        target = row["engine"] - part
        ratio = charge / target if target > 0 else float("nan")
        print(f"{row['name'][:50]:50s} {row['engine']:9.1f} {row['ree']:9.1f} {part:8.1f} {charge:9.1f} {target:9.1f} {ratio:6.2f}{' H' if row['heldOut'] else ''}")


# Rows the library's own loops dominate: their prices come from `library`, not from an instruction fit
LIBRARY_BOUND = {"string.format", "string.tostring", "string.concat", "string.byte_sub", "call.pcall"}


def nnls_fit(rows, design, free):
    """Non-negative least squares on relative error over the rows that are not held out: the free prices and every row's model"""
    import numpy as np
    from scipy.optimize import nnls
    fit_rows = [row for row in rows if not row["heldOut"]]
    a, y, w = [], [], []
    for row in fit_rows:
        x, rest = design(row)
        a.append([x[k] for k in free])
        y.append(row["engine"] - rest)
        w.append(1.0 / row["engine"])
    a, y, w = np.array(a), np.array(y), np.array(w)
    fitted = dict(zip(free, nnls(a * w[:, None], y * w)[0]))
    models = {}
    for row in rows:
        x, rest = design(row)
        models[row["name"]] = rest + sum(fitted[k] * x[k] for k in free)
    return fitted, models


def report(label, rows, fitted, models):
    print(f"{label}: fitted on {sum(1 for r in rows if not r['heldOut'])} rows: " + ", ".join(f"{key} {value:.2f}" for key, value in fitted.items()))
    errors = {"fit": [], "held out": []}
    for row in sorted(rows, key=lambda r: r["name"]):
        error = models[row["name"]] / row["engine"]
        errors["held out" if row["heldOut"] else "fit"].append((abs(error - 1) * 100, row["name"]))
        print(f"  {row['name']:40s} engine {row['engine']:10.2f} model {models[row['name']]:10.2f} x{error:5.2f} (ree now x{row['ratio']:5.2f}){' H' if row['heldOut'] else ''}")
    for side, values in errors.items():
        if values:
            print(f"{side}: n={len(values)} median |error| {statistics.median(v for v, _ in values):.1f} %, worst {max(values)[0]:.1f} % ({max(values)[1]})")


def luau(args):
    family = "native" if args.native else "lang"
    rows = [row for row in rows_of(args.ree, args.engine, family) if row.get("work") and row["unit"] == "ns"
            and row["name"].split(".", 1)[1] not in LIBRARY_BOUND]
    key = "nativeops" if args.native else "ops"
    extra = args.free.split(",") if args.free else ([] if args.native else ["slowimport"])
    free = list(CLASSES) + extra

    def design(row):
        f = features(row["work"], row["n"])
        x = {**f, **classes(row["work"], row["n"], key)}
        # (the other kind of instruction, and every count not fitted, at cost.rs's prices)
        other = classes(row["work"], row["n"], "ops" if args.native else "nativeops")
        rest = (sum(LUAU[k] * f[k] for k in LUAU if k not in extra) + f["hostnanos"]
                + sum((INTERPRETED if args.native else NATIVE)[c] * other[c] for c in CLASSES))
        return x, rest

    fitted, models = nnls_fit(rows, design, free)
    report(family, rows, fitted, models)


def library(args):
    rows = [row for row in rows_of(args.ree, args.engine, None) if row.get("work") and row["unit"] == "ns"
            and any(features(row["work"], row["n"])[k] for k in LIBRARY)]
    free = list(LIBRARY)

    def design(row):
        f = features(row["work"], row["n"])
        return f, luau_part(row["work"], row["n"], prices={k: v for k, v in LUAU.items() if k not in LIBRARY})

    fitted, models = nnls_fit(rows, design, free)
    report("library", rows, fitted, models)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    r = sub.add_parser("residuals")
    r.add_argument("ree")
    r.add_argument("engine", nargs="+")
    r.add_argument("--family")
    f = sub.add_parser("luau")
    f.add_argument("ree")
    f.add_argument("engine", nargs="+")
    f.add_argument("--native", action="store_true")
    f.add_argument("--free", help="other prices to fit with the instruction classes (default: slowimport, interpreted; none, native)")
    b = sub.add_parser("library")
    b.add_argument("ree")
    b.add_argument("engine", nargs="+")
    args = parser.parse_args()
    {"residuals": residuals, "luau": luau, "library": library}[args.command](args)


if __name__ == "__main__":
    main()
