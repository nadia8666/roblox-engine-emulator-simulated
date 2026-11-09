"""The parity bench: what a workload costs in ree's work clock against what it costs in the real engine.

    python -I tools/cost-parity/parity.py ree [--ree target/release/ree.exe] [--families a,b] [--device phone] [--out ree.json]
    python -I tools/cost-parity/parity.py job [--families a,b] > job.luau        the engine side's job text, to run in a client
    python -I tools/cost-parity/parity.py compare ree.json engine.json [engine2.json ...] [--family x] [--all]
    python -I tools/cost-parity/parity.py --selftest

Corpus.luau holds the workloads (engine calls by family, GC and memory) and Language.luau the language-level ones, which
run twice: interpreted and as a `--!native` module. One job text runs on both sides: it makes the three ModuleScripts
from their sources, requires them and returns every workload's rounds. In the engine os.clock is the real time; in ree
it is the work clock (src/cost.rs), so a row's ratio ree / engine is how far ree's price of that workload is off.
The engine side is any client that can set ModuleScript.Source (a plugin-level command bar in a playtest client, or a
job at identity 4) and returns the job's value as JSON; `compare` takes one or more such files (several engine runs give
the noise: each row's spread between runs is printed beside its error).

A third of the workloads, chosen by a hash of their names, is held out: `compare` reports it apart, and a price fitted
on these rows (src/cost.rs) must be fitted on the other two thirds only (`--fit-rows` lists them).
Standard library only.
"""
import argparse
import hashlib
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
FAMILIES = ["loop", "read", "write", "method", "find", "attr", "create", "clone", "parent", "destroy", "signal", "task", "tween",
            "datatype", "lib", "libsize", "json", "gc", "mem", "lang", "native"]
# Families a sim job runs together (a job is capped at 300 s; each group stays well under it)
GROUPS = [["loop", "read", "attr"], ["write"], ["method", "find"], ["create", "clone"], ["parent", "destroy"], ["signal", "task", "tween"],
          ["datatype"], ["lib"], ["libsize"], ["json"], ["gc"], ["mem"], ["lang"], ["native"]]


def held_out(name):
    """A third of the workloads, by a hash of the name: never used to set a price."""
    return int(hashlib.sha1(name.encode()).hexdigest(), 16) % 3 == 0


def long_string(text):
    """`text` as a Luau long string whose level no line of it closes."""
    level = 1
    while ("]" + "=" * level + "]") in text:
        level += 1
    return "[" + "=" * level + "[\n" + text + "]" + "=" * level + "]"


def job_text(families, rounds=9, printer="print"):
    """The job both sides run: the corpus and the language modules made from their sources (as is, `--!native`, and one whose
    environment getfenv made unsafe), then Corpus.Run."""
    corpus = (HERE / "Corpus.luau").read_text(encoding="utf-8")
    language = (HERE / "Language.luau").read_text(encoding="utf-8")
    family_list = "{" + ", ".join(f'"{f}"' for f in families) + "}"
    return f"""local Sources = {{ Corpus = {long_string(corpus)}, Language = {long_string(language)} }}
local Folder = Instance.new("Folder")
Folder.Name = "ParityBench"
Folder.Parent = game:GetService("ReplicatedStorage")
local function Module(Name, Source)
	local M = Instance.new("ModuleScript")
	M.Name = Name
	M.Source = Source
	M.Parent = Folder
	return M
end
local Corpus = require(Module("Corpus", Sources.Corpus))
local Language = {{ interp = require(Module("Language", Sources.Language)), native = require(Module("LanguageNative", "--!native\\n" .. Sources.Language)),
	unsafe = require(Module("LanguageUnsafe", "local _ = getfenv()\\n" .. Sources.Language)) }}
task.wait()
local Result = Corpus.Run({{ Families = {family_list}, Rounds = {rounds}, Language = Language, Print = {printer} }})
-- (a client runs each group of families as a job of one session: each job takes its scene out again)
Corpus.Cleanup()
Folder:Destroy()
return Result
"""


def ree_harness(families, rounds):
    """The job as a ree harness: it prints the result as one line and ends the run (`@done`)."""
    body = job_text(families, rounds)
    body = body.replace("\nreturn Result\n", '\nprint("PARITY-RESULT " .. game:GetService("HttpService"):JSONEncode(Result))\nprint("@done")\n')
    return "return function(Context)\n" + body + "end\n"


def git_head(path):
    try:
        out = subprocess.run(["git", "-C", str(path), "rev-parse", "--short", "HEAD"], capture_output=True, text=True, timeout=30)
        dirty = subprocess.run(["git", "-C", str(path), "status", "--porcelain", "--untracked-files=no"], capture_output=True, text=True, timeout=30)
        return out.stdout.strip() + ("-dirty" if dirty.stdout.strip() else "")
    except OSError:
        return None


def run_ree(args):
    ree = Path(args.ree or ROOT / "target" / "release" / ("ree.exe" if os.name == "nt" else "ree"))
    families = args.families.split(",") if args.families else FAMILIES
    with tempfile.TemporaryDirectory(prefix="parity-") as folder:
        harness = Path(folder) / "Parity.luau"
        harness.write_text(ree_harness(families, args.rounds), encoding="utf-8")
        empty = Path(folder) / "roots"
        empty.mkdir()
        env = dict(os.environ)
        env["REE_WORK_COUNTERS"] = "1"  # each row also gets the VM's counts of one round (fit.py)
        if args.device:
            env["REE_DEVICE"] = args.device
        command = [str(ree), "run", "--harness", str(harness), "--roots", str(empty), "--seconds", "3600"]
        started = time.time()
        done = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", env=env)
    if done.returncode != 0:
        sys.exit(f"parity: ree exited {done.returncode}\n{done.stderr[-3000:]}")
    summary = json.loads(done.stdout)
    result = None
    for line in summary.get("output", []):
        message = line.get("message", "")
        if message.startswith("PARITY-RESULT "):
            result = json.loads(message[len("PARITY-RESULT "):])
        elif line.get("kind") in ("error", "warning") or message.startswith("ERROR"):
            print("ree:", message[:400], file=sys.stderr)
    if result is None:
        sys.exit("parity: the ree run printed no result\n" + done.stdout[-3000:])
    result["side"] = "ree"
    result["ree"] = {"exe": str(ree), "commit": git_head(ROOT), "device": args.device or "desktop", "hostSeconds": round(time.time() - started, 2)}
    return result




def by_name(result):
    return {row["name"]: row for row in result["rows"] if "median" in row}


def value(row):
    """A row's figure: the median round (as the corpus sets `value`)."""
    return row.get("value", row["median"])


def compare(ree, engines, family=None, show_all=False, fastest=False):
    """Rows of (name, ree value, engine value, ratio, spread of the engine runs, held out). The engine's value is the median of its
    runs, or with `fastest` their least: whatever else runs on the machine only ever slows a client's session (two sessions on a
    loaded PC read most rows 1.3-1.9x apart, nearly all of it one session slower throughout)."""
    ree_rows = by_name(ree)
    engine_rows = [by_name(e) for e in engines]
    out = []
    for name, row in ree_rows.items():
        if family and row["family"] != family:
            continue
        seen = [value(e[name]) for e in engine_rows if name in e]
        if not seen:
            continue
        engine_value = min(seen) if fastest else statistics.median(seen)
        spread = (max(seen) - min(seen)) / engine_value if len(seen) > 1 and engine_value else None
        ratio = value(row) / engine_value if engine_value else None
        out.append({"name": name, "family": row["family"], "unit": row.get("unit", "ns"), "ree": value(row), "engine": engine_value,
                    "ratio": ratio, "spread": spread, "heldOut": held_out(name), "work": row.get("work")})
    return out


def error_stats(rows):
    errors = [abs(r["ratio"] - 1) * 100 for r in rows if r["ratio"] is not None]
    if not errors:
        return None
    worst = max(rows, key=lambda r: abs(r["ratio"] - 1) if r["ratio"] is not None else -1)
    return {"n": len(errors), "median": statistics.median(errors), "mean": statistics.fmean(errors), "worst": max(errors), "worstName": worst["name"],
            "within25": sum(e <= 25 for e in errors), "within50": sum(e <= 50 for e in errors)}


def print_table(rows, ree, engines, show_all):
    print(f"ree {ree.get('ree', {}).get('commit')} ({ree.get('ree', {}).get('device', 'desktop')}) against "
          + ", ".join(f"engine {e.get('engine', {}).get('version') or e.get('meta', {}).get('version')} ({e.get('engine', {}).get('place', '?')})" for e in engines))
    print(f"{'workload':52s} {'unit':5s} {'engine':>10s} {'ree':>10s} {'ree/eng':>8s} {'noise':>6s}")
    for r in sorted(rows, key=lambda r: (r["family"], r["name"])):
        if not show_all and r["ratio"] is not None and abs(r["ratio"] - 1) < 0.25:
            continue
        noise = f"{r['spread'] * 100:5.0f}%" if r["spread"] is not None else "     -"
        mark = " (held out)" if r["heldOut"] else ""
        print(f"{r['name']:52s} {r['unit'][:5]:5s} {r['engine']:10.1f} {r['ree']:10.1f} {r['ratio']:8.2f} {noise}{mark}")
    for label, subset in (("fit rows", [r for r in rows if not r["heldOut"]]), ("held out", [r for r in rows if r["heldOut"]]), ("all", rows)):
        stats = error_stats(subset)
        if stats:
            print(f"{label:9s} n={stats['n']:3d}  |error| median {stats['median']:6.1f} %  mean {stats['mean']:6.1f} %  worst {stats['worst']:7.1f} % ({stats['worstName']})"
                  f"  within 25 %: {stats['within25']}, within 50 %: {stats['within50']}")
    families = sorted({r["family"] for r in rows})
    print("by family (median |error| %, n):", ", ".join(f"{f} {error_stats([r for r in rows if r['family'] == f])['median']:.0f} ({len([r for r in rows if r['family'] == f])})" for f in families))


def load(path):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    return data if isinstance(data, list) else [data]


def selftest():
    names = [f"w{i}" for i in range(300)]
    share = sum(held_out(n) for n in names) / len(names)
    assert 0.25 < share < 0.42, share
    assert held_out("read.Part.CFrame") == held_out("read.Part.CFrame")
    text = job_text(["read"])
    assert "Corpus.Run" in text and '"--!native\\n" .. Sources.Language' in text
    assert "Corpus.Cleanup()" in text
    assert long_string("a ]] b ]=] c").startswith("[==[")
    harness = ree_harness(["read"], 3)
    assert harness.startswith("return function(Context)") and "PARITY-RESULT" in harness and "@done" in harness
    rows = compare({"rows": [{"name": "a", "family": "f", "median": 2.0}, {"name": "b", "family": "f", "median": 1.0}]},
                   [{"rows": [{"name": "a", "family": "f", "median": 1.0}, {"name": "b", "family": "f", "median": 1.0}]},
                    {"rows": [{"name": "a", "family": "f", "median": 3.0}]}])
    assert rows[0]["ratio"] == 1.0 and rows[0]["spread"] == 1.0 and rows[1]["ratio"] == 1.0, rows
    print("parity selftest ok")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--selftest", action="store_true")
    sub = parser.add_subparsers(dest="command")
    r = sub.add_parser("ree")
    r.add_argument("--ree")
    r.add_argument("--families")
    r.add_argument("--rounds", type=int, default=9)
    r.add_argument("--device")
    r.add_argument("--out")
    j = sub.add_parser("job")
    j.add_argument("--families")
    j.add_argument("--rounds", type=int, default=9)
    c = sub.add_parser("compare")
    c.add_argument("ree")
    c.add_argument("engine", nargs="+")
    c.add_argument("--family")
    c.add_argument("--all", action="store_true")
    c.add_argument("--json")
    c.add_argument("--fastest", action="store_true", help="each row's fastest engine run instead of their median")
    c.add_argument("--fit-rows", action="store_true", help="list the rows a price may be fitted on (not held out)")
    args = parser.parse_args()
    if args.selftest:
        return selftest()
    if args.command == "ree":
        result = run_ree(args)
        text = json.dumps(result, indent=1)
        if args.out:
            Path(args.out).write_text(text, encoding="utf-8")
        else:
            print(text)
        return 0
    if args.command == "job":
        print(job_text(args.families.split(",") if args.families else FAMILIES, args.rounds))
        return 0
    if args.command == "compare":
        ree = load(args.ree)[0]
        engines = [run for path in args.engine for run in load(path)]
        rows = compare(ree, engines, args.family, fastest=args.fastest)
        if args.fit_rows:
            for row in rows:
                if not row["heldOut"]:
                    print(row["name"])
            return 0
        print_table(rows, ree, engines, args.all)
        if args.json:
            Path(args.json).write_text(json.dumps(rows, indent=1), encoding="utf-8")
        return 0
    parser.print_help()
    return 2


if __name__ == "__main__":
    sys.exit(main())
