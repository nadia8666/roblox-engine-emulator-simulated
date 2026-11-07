# Fits src/cost.rs `luau` weights: ree's work counters a run (Bench.luau's output) against Studio's os.clock a run
# (StudioDriver.luau's JSON), and prints each workload's model beside Studio's time.
#   python -I tools/luau-cost/fit.py ree.json studio.json [--weights '{"op": 1.6, ...}']
# Without --weights it fits them (non-negative least squares on relative error, the game-like workloads weighted
# 4x, library-bound workloads and those studio.json has no time for left out) and needs numpy and scipy. The
# weights in src/cost.rs were fitted with a game's own code in `wavefill` and `waveouter`; this tree's stand-ins
# (Bench/SyntheticWave.luau) have no Studio times in studio.json, so a fit here leaves them out.
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SOURCE = (ROOT / "vendor/luau0-src/luau/Common/include/Luau/Bytecode.h").read_text()
ENUM = SOURCE[SOURCE.index("enum LuauOpcode"):]
OPS = re.findall(r"^\s*LOP_([A-Z0-9_]+),", ENUM[:ENUM.index("};")], re.M)
# (a method call's NAMECALL is followed by its CALL or CALLFB, which Luau 0.746 dispatches apart: the call is counted there)
CALLS = {"CALL", "CALLFB"}
FASTCALLS = {"FASTCALL", "FASTCALL1", "FASTCALL2", "FASTCALL2K", "FASTCALL3"}
FEATURES = ["op", "call", "fastcall", "object", "stored", "rehash", "ccall", "hostcall"]
LIBRARY = {"strsub", "format", "sort", "tconcat", "concat", "strfind", "tclone"}
GAME_LIKE = {"meshslice", "wavefill", "waveouter", "particles", "registry", "cframechain", "closures"}


def ree_runs(path):
    runs = {}
    for line in json.loads(Path(path).read_text())["output"]:
        if line["message"].startswith("BENCH "):
            run = json.loads(line["message"][6:])
            run["ops"] = {OPS[int(op)]: n for op, n in run["ops"].items()}
            runs[run["name"]] = run
    return runs


def features(run):
    ops = run["ops"]
    count = lambda names: sum(n for op, n in ops.items() if op in names)
    return [sum(ops.values()), count(CALLS), count(FASTCALLS), run["objects"], run["stored"], run["rehashed"], run["ccalls"], run["hostcalls"]]


def main():
    runs, studio = ree_runs(sys.argv[1]), json.loads(Path(sys.argv[2]).read_text())
    if "--weights" in sys.argv:
        given = json.loads(sys.argv[sys.argv.index("--weights") + 1])
        weights = [given[name] * 1e-9 for name in FEATURES]
    else:
        import numpy as np
        from scipy.optimize import nnls
        names = [name for name in runs if name not in LIBRARY and name in studio]
        a = np.array([features(runs[name]) for name in names])
        y = np.array([studio[name]["median"] for name in names])
        w = np.array([(4 if name in GAME_LIKE else 1) / studio[name]["median"] for name in names])
        weights = list(nnls(a * w[:, None], y * w)[0])
    for name, weight in zip(FEATURES, weights):
        print(f"{name:9s} {weight * 1e9:7.3f} ns")
    for name, run in runs.items():
        if name not in studio:
            print(f"{name:13s} no Studio time")
            continue
        model = sum(f * w for f, w in zip(features(run), weights))
        seen = studio[name]["median"]
        print(f"{name:13s} studio {seen * 1e9:10.1f} ns  model {model * 1e9:10.1f}  x{model / seen:5.2f}")


main()
