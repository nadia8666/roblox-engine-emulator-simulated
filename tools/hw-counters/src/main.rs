//! ree's counter service: counts the CPU's hardware events (instructions, cycles, cycles stalled on loads, the data
//! cache's misses outstanding, branch mispredictions) for
//! `ree ... --profile hw`, for the asking process's own threads only (docs/cli.md "Hardware counters").
//!
//! A process cannot read the CPU's counters on Windows, and the kernel trace session that can needs the system's
//! profiling right, which an account's ordinary processes never hold: so this service, installed once from an elevated
//! shell (setup.ps1), runs that session on behalf of the one account it was installed for.
//!
//! ```text
//! ree-hw-counters service --allow <SID>   what the service manager runs (setup.ps1 installs it)
//! ree-hw-counters serve [--allow <SID>]   the same in a console, on a pipe of its own (REE_HW_COUNTERS=console)
//! ree-hw-counters probe [--console]       checks each counter on kernels whose counts are known
//! ree-hw-counters sources                 the counters the CPU offers
//! ree-hw-counters profile                 the counter definitions it registers with Windows (a WPR profile)
//! ```

// (the wire format and the client are ree's own: src/profiler/hw)
#[path = "../../../src/profiler/hw/client.rs"]
#[allow(dead_code)]
mod client;
#[path = "../../../src/profiler/hw/counters.rs"]
#[allow(dead_code)]
mod counters;
#[path = "../../../src/profiler/hw/protocol.rs"]
#[allow(dead_code)]
mod protocol;
#[path = "../../../src/profiler/hw/readings.rs"]
#[allow(dead_code)]
mod readings;

#[cfg(windows)]
mod etw;
#[cfg(windows)]
mod ffi;
#[cfg(all(windows, target_arch = "x86_64"))]
mod probe;
#[cfg(windows)]
mod register;
#[cfg(windows)]
mod server;
#[cfg(windows)]
mod service;

const USAGE: &str = "usage: ree-hw-counters service --allow <SID> | serve [--allow <SID>] | probe [--console] | sources | profile";

#[cfg(windows)]
fn allowed(args: &[String]) -> Result<ffi::Sid, String> {
    match args.iter().position(|arg| arg == "--allow").and_then(|index| args.get(index + 1)) {
        Some(text) => ffi::Sid::parse(text),
        None => ffi::Sid::of_process(unsafe { ffi::GetCurrentProcess() }),
    }
}

#[cfg(windows)]
static CONSOLE_STOP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Ctrl+C in `serve`: the server stops, and its session with it
#[cfg(windows)]
unsafe extern "system" fn on_console_control(_kind: u32) -> i32 {
    let stop = CONSOLE_STOP.load(std::sync::atomic::Ordering::Acquire) as ffi::Handle;
    if !stop.is_null() {
        unsafe { ffi::SetEvent(stop) };
    }
    1
}

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("service") => match allowed(&args).and_then(service::run) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("ree-hw-counters: {error}");
                1
            }
        },
        Some("serve") => {
            let result = allowed(&args).and_then(|allowed| {
                let stop = unsafe { ffi::CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
                CONSOLE_STOP.store(stop as usize, std::sync::atomic::Ordering::Release);
                unsafe { ffi::SetConsoleCtrlHandler(Some(on_console_control), 1) };
                // (held only in an elevated console: then this counts as the service does)
                ffi::enable_privilege("SeSystemProfilePrivilege");
                eprintln!("ree-hw-counters: serving {} for {} (Ctrl+C stops)", protocol::CONSOLE_PIPE_NAME, allowed.text());
                server::run(&allowed, protocol::CONSOLE_PIPE_NAME, etw::CONSOLE_SESSION_NAME, stop, Some(std::time::Duration::from_secs(600)), &|line| eprintln!("ree-hw-counters: {line}"))
            });
            match result {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("ree-hw-counters: {error}");
                    1
                }
            }
        }
        #[cfg(target_arch = "x86_64")]
        Some("probe") => probe::run(if args.iter().any(|arg| arg == "--console") { client::Endpoint::Console } else { client::Endpoint::Service }),
        Some("sources") => match etw::sources() {
            Ok(list) => {
                println!("{:>4}  {:<32} {:>10} {:>12}", "id", "name", "min", "max");
                for (id, name, min, max) in &list {
                    println!("{id:>4}  {name:<32} {min:>10} {max:>12}");
                }
                println!("at once: {}", etw::max_counters().map_or("?".into(), |max| max.to_string()));
                let (vendor, family) = register::cpu();
                let custom: Vec<&str> = counters::KNOWN.iter().filter(|known| known.custom.is_some()).map(|known| known.name).collect();
                println!("this CPU: {vendor} family {family:#x}; ree's AMD counters ({}) {}", custom.join(", "), if register::custom_counters_fit() { "are defined for it" } else { "are defined for AMD Family 1Ah only" });
                0
            }
            Err(error) => {
                eprintln!("ree-hw-counters: {error}");
                1
            }
        },
        Some("profile") => {
            print!("{}", register::profile());
            0
        }
        Some("version") => {
            println!("ree-hw-counters {} (protocol {})", env!("CARGO_PKG_VERSION"), protocol::VERSION);
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ree-hw-counters runs on Windows only ({USAGE})");
    std::process::exit(2);
}
