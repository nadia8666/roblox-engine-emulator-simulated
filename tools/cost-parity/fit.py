"""Prices from the parity bench: what each engine call and each kind of Luau work should cost on ree's work clock.

    python -I tools/cost-parity/fit.py residuals ree.json engine.json [engine2.json ...] [--family write]
    python -I tools/cost-parity/fit.py luau ree.json engine.json [...] [--native] [--free key,key]
    python -I tools/cost-parity/fit.py library ree.json engine.json [...]
    python -I tools/cost-parity/fit.py sizes ree.json engine.json [...] [--free key,key]

`ree.json` is `parity.py ree` output: every row carries the VM's counts of one round (REE_WORK_COUNTERS=1). The Luau part of
a row is those counts at src/cost.rs's Luau prices (mirrored below; keep them equal); the rest of the engine's time is what the
engine calls of the row cost, so `residuals` prints, per row, the engine call's price that would make the row exact beside what
ree charges now. `luau` fits the price of each kind of instruction (CLASSES) on the `lang` rows, interpreted, or the `native`
rows with --native (and with --free, those prices too), `library` the library's prices (LIBRARY) on the `lib` rows, and `sizes` the
prices of work that grows with a call's input (SIZES: elements scanned and moved, string bytes, pattern steps, buffer bytes,
allocated bytes and large blocks, JSON text and values; the collector's price an object is held, from its measured share) on the
`libsize`, `json` and `gc` rows, each workload at several sizes: non-negative least squares on relative error, using only rows that are not held out (parity.held_out); each prints every
row's error with the fitted prices, the held-out ones apart. Needs numpy and scipy for `luau`, `library` and `sizes`.
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
LIBRARY = {"numtostr": 18.0, "newstring": 171.0, "sortcompare": 1.06, "sortcall": 9.7, "createitem": 1.95, "cloneitem": 4.3,
           "concatitem": 5.0, "splitpart": 25.4, "format": 102.0, "formatspec": 0.0}
# work by the size of a call's input (src/cost.rs LUAU_SCANNED and below, and json's prices)
SIZES = {
    # table elements compared and moved, bytes of string and of pattern text scanned, pattern steps, buffer bytes
    "scanned": 0.357, "moved": 0.226, "strbyte": 0.169, "strscanned": 0.024, "matchstep": 0.796, "bufbyte": 0.004,
    # allocated bytes, objects the collector sweeps and objects library calls make
    "allocbyte": 0.061, "collectorobject": 8.6, "libobject": 5.99,
    # case conversion, utf8 and character work, buffers made, character classes tested
    "casebyte": 1.485, "utf8byte": 0.979, "utf8char": 6.71, "strchar": 2.16, "bytevalue": 1.46, "buffer": 1774.0, "matchclass": 6.33,
    # keys put back in a hash part, blocks over 1 KB, bytes counted and priced at nothing
    "hashrehash": 15.43, "largeblock": 79.8, "buffresh": 0.0, "bufbigcopy": 0.0,
    # JSON text and values, for an encode and for a decode
    "jsonencodebyte": 0.0, "jsonencodevalue": 62.1, "jsondecodebyte": 8.15, "jsondecodevalue": 1.50,
}
# a frame's cold first touches (src/cost.rs LUAU_COLD_TABLE and below): every round of a row runs in a frame of its own
COLD = {"coldtable": 200.0, "coldclosure": 30.0, "coldproto": 5.0, "coldcode": 0.85, "coldcfunc": 2500.0, "coldhostfunc": 900.0,
        "rareproto": 700.0}
LUAU = {"object": 5.99, "stored": 0.079, "rehash": 0.0, "ccall": 5.77, "hostcall": 15.0, "magnitude": 12.5, "unit": 9.1,
        "vectormethod": 0.0, "slowimport": 7.9, **LIBRARY, **SIZES, **COLD}


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
        "scanned": work.get("scanned", 0) / n,
        "moved": work.get("moved", 0) / n,
        "strbyte": work.get("strbytes", 0) / n,
        "strscanned": work.get("strscanned", 0) / n,
        "matchstep": work.get("matchsteps", 0) / n,
        "bufbyte": work.get("bufbytes", 0) / n,
        "allocbyte": work.get("allocbytes", 0) / n,
        "collectorobject": (work.get("objects", 0) + work.get("libobjects", 0)) / n,
        "libobject": work.get("libobjects", 0) / n,
        "casebyte": work.get("casebytes", 0) / n,
        "utf8byte": work.get("utf8bytes", 0) / n,
        "utf8char": work.get("utf8chars", 0) / n,
        "strchar": work.get("strchars", 0) / n,
        "bytevalue": work.get("bytevalues", 0) / n,
        "buffer": work.get("buffers", 0) / n,
        "matchclass": work.get("matchclasses", 0) / n,
        "hashrehash": work.get("hashrehashed", 0) / n,
        "largeblock": work.get("largeblocks", 0) / n,
        "buffresh": work.get("buffreshbytes", 0) / n,
        "bufbigcopy": work.get("bufbigcopybytes", 0) / n,
        # (an encode and a decode of the same text: the row's name says which)
        "jsonencodebyte": 0.0,
        "jsonencodevalue": 0.0,
        "jsondecodebyte": 0.0,
        "jsondecodevalue": 0.0,
        "coldtable": work.get("coldtables", 0) / n,
        "coldclosure": work.get("coldclosures", 0) / n,
        "coldproto": work.get("coldprotos", 0) / n,
        "coldcode": work.get("coldcode", 0) / n,
        "coldcfunc": work.get("coldcfuncs", 0) / n,
        "coldhostfunc": work.get("coldhostfuncs", 0) / n,
        "rareproto": work.get("rareprotos", 0) / n,
    }


def row_features(row):
    """features() of a row, with its JSON counts on the side its name says (an encode's or a decode's)"""
    f = features(row["work"], row["n"])
    side = "decode" if "JSONDecode" in row["name"] else "encode"
    f[f"json{side}byte"] = row["work"].get("jsonbytes", 0) / row["n"]
    f[f"json{side}value"] = row["work"].get("jsonvalues", 0) / row["n"]
    return f


def luau_part(work, n, interpreted=INTERPRETED, native=NATIVE, prices=LUAU, f=None):
    """A row's Luau work at these prices, nanoseconds per operation of the row"""
    f = f or features(work, n)
    ci, cn = classes(work, n, "ops"), classes(work, n, "nativeops")
    return (sum(prices[k] * f[k] for k in prices) + f["hostnanos"] + sum(interpreted[c] * ci[c] for c in interpreted)
            + sum(native[c] * cn[c] for c in native))


# each row's fastest engine run instead of the median (fit.py --fastest; parity.compare)
FASTEST = False


def rows_of(path_ree, paths_engine, family=None):
    ree = parity.load(path_ree)[0]
    engines = [run for path in paths_engine for run in parity.load(path)]
    rows = parity.compare(ree, engines, family, fastest=FASTEST)
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


def sizes(args):
    families = {"libsize", "json", "gc"}
    rows = [row for row in rows_of(args.ree, args.engine, None) if row.get("work") and row["unit"] == "ns" and row["family"] in families]
    free = args.free.split(",") if args.free else list(SIZES)

    def design(row):
        f = row_features(row)
        return f, luau_part(row["work"], row["n"], prices={k: v for k, v in LUAU.items() if k not in free}, f=f)

    fitted, models = nnls_fit(rows, design, free)
    report("sizes", rows, fitted, models)


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
    s = sub.add_parser("sizes")
    s.add_argument("ree")
    s.add_argument("engine", nargs="+")
    s.add_argument("--free", help="the prices to fit (default: every one of SIZES)")
    for command in (r, f, b, s):
        command.add_argument("--fastest", action="store_true", help="each row's fastest engine run instead of their median")
    args = parser.parse_args()
    global FASTEST
    FASTEST = args.fastest
    {"residuals": residuals, "luau": luau, "library": library, "sizes": sizes}[args.command](args)


if __name__ == "__main__":
    main()
