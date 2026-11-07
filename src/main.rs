//! `ree`: the engine's command line (docs/cli.md).
//!
//!   ree record --harness <file.luau> --out <dir> [options]   run a harness and record the 3D world
//!   ree run --harness <file.luau> [options]                  run a harness, print its output as JSON
//!   ree record|run --service ...                             the same, run by the warm service
//!   ree serve [--status | --stop]                            the service (src/serve)
//!   ree --version

use roblox_engine_emulator::{launch::Host, record, serve};

/// mimalloc instead of the system heap: the same results, faster allocation-heavy script code. What a
/// warm service keeps across runs goes to a heap of its own (roblox_engine_emulator::alloc).
#[cfg(not(feature = "dhat-heap"))]
#[global_allocator]
static GLOBAL: Allocator = Allocator;

#[cfg(not(feature = "dhat-heap"))]
struct Allocator;

#[cfg(not(feature = "dhat-heap"))]
mod heap {
    use std::ffi::c_void;

    // (mimalloc, linked by the mimalloc crate)
    unsafe extern "C" {
        pub fn mi_malloc(size: usize) -> *mut c_void;
        pub fn mi_zalloc(size: usize) -> *mut c_void;
        pub fn mi_realloc(p: *mut c_void, size: usize) -> *mut c_void;
        pub fn mi_malloc_aligned(size: usize, alignment: usize) -> *mut c_void;
        pub fn mi_zalloc_aligned(size: usize, alignment: usize) -> *mut c_void;
        pub fn mi_realloc_aligned(p: *mut c_void, size: usize, alignment: usize) -> *mut c_void;
        pub fn mi_free(p: *mut c_void);
        fn mi_heap_new() -> *mut c_void;
        pub fn mi_heap_malloc_aligned(heap: *mut c_void, size: usize, alignment: usize) -> *mut c_void;
        pub fn mi_heap_zalloc_aligned(heap: *mut c_void, size: usize, alignment: usize) -> *mut c_void;
        pub fn mi_heap_realloc_aligned(heap: *mut c_void, p: *mut c_void, size: usize, alignment: usize) -> *mut c_void;
        pub fn mi_option_set_enabled_default(option: i32, enable: bool);
        pub fn mi_option_set_default(option: i32, value: std::ffi::c_long);
    }

    /// mimalloc v2's `mi_option_eager_commit`
    pub const MI_OPTION_EAGER_COMMIT: i32 = 3;
    /// mimalloc v2's `mi_option_purge_delay`
    pub const MI_OPTION_PURGE_DELAY: i32 = 15;

    thread_local! {
        static HEAP: std::cell::Cell<*mut c_void> = const { std::cell::Cell::new(std::ptr::null_mut()) };
    }

    /// This thread's long-lived heap (mimalloc's heaps belong to the thread that made them; any
    /// thread frees their blocks), made on first use
    pub fn long_lived() -> *mut c_void {
        HEAP.with(|heap| {
            if heap.get().is_null() {
                heap.set(unsafe { mi_heap_new() });
            }
            heap.get()
        })
    }

    /// Whether mimalloc's plain functions give this alignment (as the mimalloc crate decides)
    #[inline]
    pub fn plain(size: usize, align: usize) -> bool {
        align <= 16 && align <= size
    }
}

#[cfg(not(feature = "dhat-heap"))]
unsafe impl std::alloc::GlobalAlloc for Allocator {
    #[inline]
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let (size, align) = (layout.size(), layout.align());
        unsafe {
            if roblox_engine_emulator::alloc::in_long_lived() {
                heap::mi_heap_malloc_aligned(heap::long_lived(), size, align).cast()
            } else if heap::plain(size, align) {
                heap::mi_malloc(size).cast()
            } else {
                heap::mi_malloc_aligned(size, align).cast()
            }
        }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        let (size, align) = (layout.size(), layout.align());
        unsafe {
            if roblox_engine_emulator::alloc::in_long_lived() {
                heap::mi_heap_zalloc_aligned(heap::long_lived(), size, align).cast()
            } else if heap::plain(size, align) {
                heap::mi_zalloc(size).cast()
            } else {
                heap::mi_zalloc_aligned(size, align).cast()
            }
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: std::alloc::Layout) {
        unsafe { heap::mi_free(ptr.cast()) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        let align = layout.align();
        unsafe {
            if roblox_engine_emulator::alloc::in_long_lived() {
                heap::mi_heap_realloc_aligned(heap::long_lived(), ptr.cast(), new_size, align).cast()
            } else if heap::plain(new_size, align) {
                heap::mi_realloc(ptr.cast(), new_size).cast()
            } else {
                heap::mi_realloc_aligned(ptr.cast(), new_size, align).cast()
            }
        }
    }
}

/// `--features dhat-heap`: every allocation recorded for a heap profile (dhat-heap.json)
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static GLOBAL: dhat::Alloc = dhat::Alloc;
// (mimalloc: the allocator above, and the service's idle collection)
extern crate mimalloc;

const USAGE: &str = "usage: ree record --harness <file.luau> --out <dir> [--seconds 3] [--fps 60] [--context <json|file>]\n                  [--assets-at <Service.Path>] [--assets <prefix>,...] [engine options]\n       ree run --harness <file.luau> [--seconds 3] [--fps 60] [--context <json|file>] [engine options]\n       ree record|run --service ...   (run by the warm service, started when missing; --profile prints timings)\n       ree run ... --profile boundary [--profile-out <file.json>] [--profile-phase <a,b|all>] [--profile-top <n>]\n                   (Luau-to-engine crossings per call site, priced; not through --service)\n       ree record|run ... --profile <kinds> [--profile-out <dir>] [--profile-top <n>] [--profile-phase <a,b|all>] [--profile-counters <a,b>]\n                   kinds: work, caches, alloc, heap, timeline, cpu, phases, time, hw, boundary, all (docs/cli.md)\n       ree profile diff <before dir> <after dir> [--out <dir>]   |   ree profile report <dir>\n       ree profile try <file.luau> [--sizes a,b] [--variants a,b] [--out <dir>]   (variants side by side)\n       ree serve [--status | --stop] [--idle <s>] [--workers <n>]\nengine options: --roots \"<dir>;<dir>\" [--project <dir>] [--place <name>], --mount Service/Name=<dir|file>,\n                --instance Service/Name=<ClassName>, --snapshot-dir <dir>, --mode play|edit, --player <name>,\n                --place-id <n>, --streaming <radius>, --viewport WxH, --offline, --verbose (docs/cli.md)";

#[cfg(feature = "dhat-heap")]
static PROFILER: std::sync::Mutex<Option<dhat::Profiler>> = std::sync::Mutex::new(None);

fn main() {
    // mimalloc commits a thread's segments page by page as they fill instead of 32 MiB at once:
    // the same allocations, ~28 MB less committed for the `--service` client and each pool thread
    // (MIMALLOC_EAGER_COMMIT still overrides it); and it keeps freed pages 100 ms before
    // decommitting them (mimalloc's default is 10 ms; 0 decommitted at once): a heavy cutscene's
    // frames free and reuse the same pages, and decommitting them at once spent ~10% of its
    // recording in commit calls and page faults, for 5-15% less peak memory
    // (MIMALLOC_PURGE_DELAY still overrides it)
    #[cfg(not(feature = "dhat-heap"))]
    unsafe {
        heap::mi_option_set_enabled_default(heap::MI_OPTION_EAGER_COMMIT, false);
        heap::mi_option_set_default(heap::MI_OPTION_PURGE_DELAY, 100);
    }
    #[cfg(feature = "dhat-heap")]
    {
        *PROFILER.lock().unwrap() = Some(dhat::Profiler::new_heap());
        let _ = roblox_engine_emulator::BEFORE_EXIT.set(|| drop(PROFILER.lock().unwrap().take()));
    }
    let argv: Vec<String> = std::env::args().collect();
    if std::env::var_os("RUR_CHARGE_TALLY").is_some() {
        roblox_engine_emulator::cost::enable_tally();
    }
    let service = argv.iter().any(|arg| arg == "--service");
    // (a service's client sizes it when it runs the command itself)
    if matches!(argv.get(1).map(String::as_str), Some("record" | "run")) && !service {
        roblox_engine_emulator::serve::size_pool();
    }
    // The command runs on a thread with a service worker's stack (64 MiB, serve/mod.rs). On the main thread's 1 MiB a
    // one-shot run ended with a stack overflow where the same job through the service ran: measured 2026-10-30,
    // GetDescendants, Clone, parenting and Destroy of a 3,000-deep instance tree, each alone of a 20,000-deep one (the
    // engine's recursion follows the tree)
    let command = {
        let argv = argv.clone();
        move || run_command(&argv, service)
    };
    let code = match std::thread::Builder::new().name("main".into()).stack_size(64 << 20).spawn(command) {
        Ok(thread) => thread.join().unwrap_or(101),
        Err(_) => run_command(&argv, service),
    };
    #[cfg(feature = "dhat-heap")]
    drop(PROFILER.lock().unwrap().take());
    std::process::exit(code);
}

/// `ree profile view|render ...`: the profile viewer, a binary of its own beside this one (profile-view/, which
/// `cargo build --release` builds with ree), so the engine's crate carries no terminal UI. Its standard streams are
/// this process's: the terminal UI runs in this terminal.
fn profile_viewer(args: &[String]) -> i32 {
    let name = format!("ree-profile-view{}", std::env::consts::EXE_SUFFIX);
    let Some(exe) = std::env::current_exe().ok().map(|exe| exe.with_file_name(&name)).filter(|exe| exe.is_file()) else {
        eprintln!("ree profile {}: the profile viewer ({name}) is not beside ree: `cargo build --release` in the checkout builds both", args[0]);
        return 2;
    };
    match std::process::Command::new(&exe).args(args).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!("ree profile {}: {}: {error}", args[0], exe.display());
            1
        }
    }
}

fn run_command(argv: &[String], service: bool) -> i32 {
    let host = Host::default();
    let code = match argv.get(1).map(String::as_str) {
        Some("record") if service => serve::client::cli(&argv[2..], true),
        Some("run") if service => serve::client::cli(&argv[2..], false),
        Some("record") => record::run::cli(&argv[2..], &host, true),
        Some("run") => record::run::cli(&argv[2..], &host, false),
        Some("serve") => serve::cli(&argv[2..]),
        Some("profile") => match argv.get(2).map(String::as_str) {
            Some("diff") => roblox_engine_emulator::profiler::diff::cli(&argv[3..]),
            Some("report") => roblox_engine_emulator::profiler::diff::report_cli(&argv[3..]),
            Some("view" | "render") => profile_viewer(&argv[2..]),
            Some("try") => roblox_engine_emulator::profiler::whatif::cli(&argv[3..]),
            _ => {
                eprintln!("usage: ree profile diff <before dir> <after dir> [--out <dir>] [--top <n>]
       ree profile report <profile dir>
       ree profile view <dir> [<dir2>]   |   ree profile render <dir> [<dir2>] --view <name|all> --out <file|dir>
       ree profile try <file.luau> [--sizes a,b] [--variants a,b] [--out <dir>]   (docs/cli.md \"What-if comparisons\")");
                2
            }
        },
        Some("--version" | "-V" | "version") => {
            println!("ree {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    // (the tally is the command thread's)
    roblox_engine_emulator::cost::dump_tally();
    code
}
