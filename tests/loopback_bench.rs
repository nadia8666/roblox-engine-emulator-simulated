//! The loopback bench (src/loopback): what a loopback pair costs against a plain run, row by row as the plan's speed table
//! has them, timed in this process on mimalloc as `ree` runs (no process noise, no system heap).
//! `cargo test --release --test loopback_bench -- --ignored --nocapture`. Each row is the median of nine repeats, each a
//! fresh setup timed against the same frames without the work; the workloads are tools/bench/loopback/work.luau.

use std::time::Instant;

use roblox_engine_emulator::{Emulator, Engine, Options, RunContext, loopback, mount::ProjectSpec};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn options(context: RunContext) -> Options {
    Options {
        mode: "play".into(),
        run_context: context,
        local_player_name: if context == RunContext::Server { None } else { Some("Player".into()) },
        ..Options::default()
    }
}

/// A plain engine of `context` with an empty project, its modules interpreted as `ree run` loads them
fn plain(context: RunContext) -> Emulator {
    let engine = Engine::new(options(context)).unwrap();
    roblox_engine_emulator::lua::modules::interpret_modules(&engine);
    Emulator::start(engine, &ProjectSpec::default()).unwrap()
}

/// A joined loopback pair with an empty project: the client's Emulator (it holds the server)
fn pair() -> Emulator {
    let client = loopback::start(&Engine::new, &options(RunContext::Loopback), &ProjectSpec::default(), loopback::Spec::default(), &|_, _| {}).unwrap();
    loopback::join(&client.lua, &client.engine).unwrap();
    client
}

fn on_server(client: &Emulator, name: &str, source: &str) {
    client.engine.run_on_server(name, source).unwrap();
}

const REPEATS: usize = 9;

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

/// Seconds `frames` steps of `emulator` take (its pair's, for a loopback client)
fn time_frames(emulator: &Emulator, frames: u32) -> f64 {
    let started = Instant::now();
    for _ in 0..frames {
        emulator.step(1.0 / 60.0).unwrap();
    }
    started.elapsed().as_secs_f64()
}

/// A workload's cost per operation: the median over fresh setups of (its frames' time - the same frames idle) / ops
fn per_op(setup: &dyn Fn() -> Emulator, frames: u32, ops: f64, work: &dyn Fn(&Emulator)) -> f64 {
    let mut samples = Vec::new();
    for _ in 0..REPEATS {
        let idle = setup();
        time_frames(&idle, 5);
        let idle_time = time_frames(&idle, frames);
        drop(idle);
        let busy = setup();
        work(&busy);
        time_frames(&busy, 2);
        let busy_time = time_frames(&busy, frames);
        drop(busy);
        samples.push((busy_time - idle_time) / ops);
    }
    median(samples)
}

const WORK: &str = include_str!("../tools/bench/loopback/work.luau");

fn work(call: &str) -> String {
    WORK.replace("--WORKLOAD--", call)
}

fn row(rows: &mut Vec<String>, what: &str, value: String, target: &str, ok: bool) {
    rows.push(format!("| {what} | {value} | {target} | {} |", if ok { "yes" } else { "NO" }));
}

#[test]
#[ignore]
fn loopback_bench() {
    let us = |seconds: f64| seconds * 1e6;
    let ns = |seconds: f64| seconds * 1e9;
    let mut rows = vec!["| row | measured | v1 target | meets |".to_string(), "|---|---|---|---|".to_string()];
    // warm the process's caches (reflection, engine Luau bytecode)
    drop(pair());
    drop(plain(RunContext::Server));

    // starting: the pair against a plain server and a plain client, empty project
    let start = |make: &dyn Fn() -> Emulator| {
        median(
            (0..REPEATS)
                .map(|_| {
                    let started = Instant::now();
                    let made = make();
                    let took = started.elapsed().as_secs_f64();
                    drop(made);
                    took
                })
                .collect(),
        )
    };
    let server_start = start(&|| plain(RunContext::Server));
    let client_start = start(&|| plain(RunContext::Client));
    let pair_start = start(&pair);
    row(&mut rows, "the second engine: pair start - plain server start (empty project)", format!("{:.2} ms ({:.2} - {:.2})", (pair_start - server_start) * 1e3, pair_start * 1e3, server_start * 1e3), "<= 5 ms", pair_start - server_start <= 0.005);
    row(&mut rows, "pair start - plain client start (empty project)", format!("{:+.2} ms (client {:.2})", (pair_start - client_start) * 1e3, client_start * 1e3), "about the same or less", pair_start - client_start <= 0.002);

    // an idle frame: the pair's against a client's and a server's
    let frames = 20_000;
    let idle = |make: &dyn Fn() -> Emulator| {
        median(
            (0..REPEATS)
                .map(|_| {
                    let emulator = make();
                    time_frames(&emulator, 100);
                    time_frames(&emulator, frames) / frames as f64
                })
                .collect(),
        )
    };
    let (idle_server, idle_client, idle_pair) = (idle(&|| plain(RunContext::Server)), idle(&|| plain(RunContext::Client)), idle(&pair));
    let extra = idle_pair - idle_server - idle_client;
    row(&mut rows, "idle frame: pair - (server + client)", format!("{:+.0} ns (pair {:.2} us, server {:.2}, client {:.2})", ns(extra), us(idle_pair), us(idle_server), us(idle_client)), "<= +50 ns", extra <= 50e-9);

    // FireServer with no arguments and one handler, host time per call (send, deliver, the handler's start), against a
    // BindableEvent:Fire with one handler in a plain client
    let calls = 1000.0;
    let fire_frames = 50;
    let fire = |argument: &'static str| {
        per_op(&pair, fire_frames, calls * fire_frames as f64, &|client: &Emulator| {
            on_server(client, "Bench", &work("Handler()"));
            client.run("Bench", &work(&format!("Fire({argument})"))).unwrap();
        })
    };
    let fire_none = fire("nil");
    let bindable = per_op(&|| plain(RunContext::Client), fire_frames, calls * fire_frames as f64, &|client: &Emulator| {
        client.run("Bench", &work("Bindable()")).unwrap();
    });
    row(&mut rows, "FireServer, no arguments, one handler (per call)", format!("{:.2} us (Bindable {:.2})", us(fire_none), us(bindable)), "<= 1.0 us", fire_none <= 1.0e-6);
    // each table entry: the slope between a 100- and a 1000-number array (a call's own cost cancels)
    let fire_100 = fire("Array100");
    let fire_1000 = fire("Array1000");
    let entry = (fire_1000 - fire_100) / 900.0;
    row(&mut rows, "each copied table entry (numbers; 1000 - 100 entries)", format!("{:.0} ns (a 100-entry table's call {:.2} us over none)", ns(entry), us(fire_100 - fire_none)), "<= 60 ns", entry <= 60e-9);
    let fire_buffer = fire("Buffer1K");
    row(&mut rows, "a 1 KB buffer (over no arguments)", format!("{:.2} us", us(fire_buffer - fire_none)), "<= 0.6 us", fire_buffer - fire_none <= 0.6e-6);

    // InvokeServer, one after another: host time per round trip without the callback's own work
    let invokes = 2000.0;
    let invoke = per_op(&pair, 4100, invokes, &|client: &Emulator| {
        on_server(client, "Bench", &work("Callback()"));
        client.run("Bench", &work("Invoke(2000)")).unwrap();
    });
    row(&mut rows, "InvokeServer round trip (host time, per call)", format!("{:.2} us", us(invoke)), "<= 2.5 us", invoke <= 2.5e-6);

    // property writes on the server: to an instance outside the replicated containers (the hooks' flag test), and to a
    // replicated one, end to end (the client's write included), each against a plain server's same writes
    let writes = 2000.0;
    let write_frames = 50;
    let server_writes = |container: &'static str| {
        let workload = work(&format!("Write(\"{container}\")"));
        let paired = per_op(&pair, write_frames, writes * write_frames as f64, &|client: &Emulator| on_server(client, "Bench", &workload));
        let alone = per_op(&|| plain(RunContext::Server), write_frames, writes * write_frames as f64, &|server: &Emulator| {
            server.run("Bench", &workload).unwrap();
        });
        (paired, alone)
    };
    let (hidden, hidden_plain) = server_writes("ServerStorage");
    row(&mut rows, "a server write to a non-replicated instance: + over plain", format!("{:+.0} ns ({:.0} vs {:.0})", ns(hidden - hidden_plain), ns(hidden), ns(hidden_plain)), "<= +15 ns", hidden - hidden_plain <= 15e-9);
    let (shown, shown_plain) = server_writes("ReplicatedStorage");
    row(&mut rows, "a replicated write, end to end: + over plain", format!("{:+.0} ns", ns(shown - shown_plain)), "<= +200 ns", shown - shown_plain <= 200e-9);

    // replicated creations: 2000 parts the server makes in the Workspace in one frame; the server's flush and the
    // client's frame that makes them (against an idle client frame), per part
    let (mut flushes, mut applies) = (Vec::new(), Vec::new());
    let parts = 2000.0;
    for _ in 0..REPEATS {
        let client = pair();
        client.step(1.0 / 60.0).unwrap();
        let (_, server) = client.engine.peer_server().unwrap();
        on_server(&client, "Make", "for _ = 1, 2000 do local P = Instance.new('Part') P.Anchored = true P.Parent = workspace end");
        let bridge = client.engine.loopback().unwrap().bridge.clone();
        let started = Instant::now();
        loopback::replicate::flush(&server, &bridge);
        flushes.push(started.elapsed().as_secs_f64() / parts);
        bridge.seal(loopback::Side::Server);
        let started = Instant::now();
        roblox_engine_emulator::scheduler::step_one(&client.lua, &client.engine, Some(1.0 / 60.0)).unwrap();
        let made = started.elapsed().as_secs_f64();
        let started = Instant::now();
        roblox_engine_emulator::scheduler::step_one(&client.lua, &client.engine, Some(1.0 / 60.0)).unwrap();
        let idle = started.elapsed().as_secs_f64();
        applies.push((made - idle) / parts);
        drop(server);
    }
    let (flush, apply) = (median(flushes), median(applies));
    row(&mut rows, "a replicated creation, on the client (per instance)", format!("{:.2} us (and the server's flush {:.2} us)", us(apply), us(flush)), "<= 0.3 us", apply <= 0.3e-6);

    eprintln!("\n{}\n", rows.join("\n"));
}
