//! ree's profile viewer: a keyboard-driven terminal UI over the profile directories `--profile` writes (one, a
//! before/after pair, or a what-if comparison's), and the same screens rendered headless to PNG, SVG or text, so an
//! agent can take a screenshot of any state the keys reach (docs/cli.md "The profile viewer").
//!
//! It only reads profile directories: profile.json, the folded stacks, boundary.json and compare.json. The engine's
//! crate does not depend on it; `ree profile view|render` runs its binary (ree-profile-view, built with the workspace),
//! and a host that embeds the engine (rur) links this library and calls [`cli::view`] and [`cli::render`].
//!
//! The screen is drawn into a cell buffer (ratatui's) by [`ui::draw_buffer`]: the terminal shows that buffer, and
//! [`export`] writes the same buffer as a file, so a screenshot is exactly what the terminal shows.

pub mod app;
pub mod cli;
pub mod compare;
pub mod diff;
pub mod export;
pub mod fmt;
pub mod issues;
pub mod keys;
pub mod metric;
pub mod model;
pub mod sites;
pub mod terminal;
pub mod theme;
pub mod tree;
pub mod ui;

/// `view` or `render` with their arguments (`args[0]` the subcommand): `program` names the command in messages
/// (`ree profile`, `rur profile`)
pub fn main(program: &str, args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("view") => cli::view(program, &args[1..]),
        Some("render") => cli::render(program, &args[1..]),
        Some("--help" | "-h" | "help") => {
            println!("{}", cli::usage(program));
            0
        }
        _ => {
            eprintln!("{}", cli::usage(program));
            2
        }
    }
}
