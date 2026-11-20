//! A loopback pair's allocations (src/loopback): an idle frame of the pair allocates nothing beyond what its two engines'
//! own idle frames allocate (the bridge's empty batches are one compare each), and what the second engine and the join's
//! copy hold. This binary counts its own thread's allocations with a global allocator of its own.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

use roblox_engine_emulator::{Emulator, Engine, Options, RunContext, loopback, mount::ProjectSpec};

// (the engine's service code calls mimalloc's collection, which the CLI links; the heap here is the counting one below)
extern crate mimalloc;

struct Counting;

thread_local! {
    static COUNT: Cell<u64> = const { Cell::new(0) };
    static LIVE: Cell<i64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = COUNT.try_with(|count| count.set(count.get() + 1));
        let _ = LIVE.try_with(|live| live.set(live.get() + layout.size() as i64));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let _ = LIVE.try_with(|live| live.set(live.get() - layout.size() as i64));
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let _ = COUNT.try_with(|count| count.set(count.get() + 1));
        let _ = LIVE.try_with(|live| live.set(live.get() + size as i64 - layout.size() as i64));
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocations() -> u64 {
    COUNT.with(Cell::get)
}

fn live() -> i64 {
    LIVE.with(Cell::get)
}

fn options(context: RunContext) -> Options {
    Options {
        mode: "play".into(),
        run_context: context,
        local_player_name: if context == RunContext::Server { None } else { Some("Player".into()) },
        ..Options::default()
    }
}

fn plain(context: RunContext) -> Emulator {
    Emulator::start(Engine::new(options(context)).unwrap(), &ProjectSpec::default()).unwrap()
}

fn pair() -> Emulator {
    let client = loopback::start(&Engine::new, &options(RunContext::Loopback), &ProjectSpec::default(), loopback::Spec::default(), &|_, _| {}).unwrap();
    loopback::join(&client.lua, &client.engine).unwrap();
    client
}

/// Allocations over `frames` idle frames of `emulator`, after it settled
fn idle_allocations(emulator: &Emulator, frames: u32) -> u64 {
    for _ in 0..50 {
        emulator.step(1.0 / 60.0).unwrap();
    }
    let before = allocations();
    for _ in 0..frames {
        emulator.step(1.0 / 60.0).unwrap();
    }
    allocations() - before
}

#[test]
fn an_idle_loopback_frame_allocates_nothing_of_its_own() {
    let (server, client, pair) = (plain(RunContext::Server), plain(RunContext::Client), pair());
    let (server, client, pair) = (idle_allocations(&server, 100), idle_allocations(&client, 100), idle_allocations(&pair, 100));
    eprintln!("allocations over 100 idle frames: server {server}, client {client}, pair {pair}");
    assert!(pair <= server + client, "the pair allocated {pair} where its engines alone allocate {server} + {client}");
}

/// The memory rows of the loopback bench: the second engine's heap, what the join's copy holds per instance
#[test]
#[ignore]
fn loopback_memory() {
    // (warm the process: the reflection, the engine Luau's bytecode)
    drop(pair());
    let before = live();
    let server = plain(RunContext::Server);
    let one = live() - before;
    drop(server);
    let before = live();
    let both = pair();
    let two = live() - before;
    eprintln!("| the second engine (and the link) | {:.2} MB (a server alone {:.2} MB) | <= 2 MB |", (two - one) as f64 / 1e6, one as f64 / 1e6);
    drop(both);
    // the join's copy: a server with 10,000 parts in ReplicatedStorage, joined
    let client = loopback::start(&Engine::new, &options(RunContext::Loopback), &ProjectSpec::default(), loopback::Spec::default(), &|_, _| {}).unwrap();
    client.engine.run_on_server("Parts", "local F = Instance.new('Folder') for I = 1, 10000 do local P = Instance.new('Part') P.Name = 'P' .. I P.Parent = F end F.Parent = game:GetService('ReplicatedStorage')").unwrap();
    let (before, instances) = (live(), client.engine.state.borrow().dom.live_count());
    loopback::join(&client.lua, &client.engine).unwrap();
    let (copy, made) = (live() - before, client.engine.state.borrow().dom.live_count() - instances);
    assert!(made >= 10_000, "the join made {made} instances");
    // (against the same parts cloned on the server: what Roblox's own copy of an instance costs here)
    let before = live();
    client.engine.run_on_server("Clone", "_G.Kept = game:GetService('ReplicatedStorage').Folder:Clone()").unwrap();
    let cloned = live() - before;
    eprintln!("| the join's copy, per made instance (10,000 parts, {made} made) | {:.0} B (Clone {:.0} B) | <= 400 B |", copy as f64 / made as f64, cloned as f64 / 10_001.0);
}
