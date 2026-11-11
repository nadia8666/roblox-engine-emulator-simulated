// Cargo starts this in place of each test binary. It hands the command line to the runner (test-runner.mjs) and exits
// with the runner's code. It is a file of its own so that the runner never has to check whether it was started as a
// script: a runner that skipped its work would exit 0 without running the test binary, and every test binary would pass.
import { main } from "../release/common.mjs";
import { run } from "./test-runner.mjs";

await main(() => { process.exitCode = run(process.argv.slice(2)); });
