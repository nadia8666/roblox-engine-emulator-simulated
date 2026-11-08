//! The digest of what decides a converted bundle (project/plan.rs, `<bundle>.plan` beside the bundle
//! files): the conversion and its file format, the reflection it resolves names and classes with,
//! the compiled-in dump and the decoders' versions. A plan file written by another build is used
//! when the digest is the same (a rebuild that touched none of these keeps every plan), and
//! converted again when it is not. Line endings are left out, so checkouts that differ only there
//! agree.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// The files and directories (every file below) a conversion depends on.
/// (src/world/frame.rs and lune-roblox's float32 module store a received part's or joint's rotation: project/received.rs)
const INPUTS: &[&str] = &[
    "build.rs",
    "Cargo.lock",
    "data",
    "src/lib.rs",
    "src/reflection.rs",
    "src/value.rs",
    "src/project.rs",
    "src/project",
    "src/world/frame.rs",
    "vendor/lune-roblox/src/datatypes/types/mod.rs",
];

fn files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(path).map(|read| read.flatten().map(|entry| entry.path()).collect()).unwrap_or_default();
        entries.sort();
        for entry in entries {
            files(&entry, out);
        }
    } else if path.is_file() {
        out.push(path.to_path_buf());
    }
}

/// FNV-1a, 64 bits, from `basis`.
fn fnv(basis: u64, bytes: impl Iterator<Item = u8>) -> u64 {
    bytes.fold(basis, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let mut list = Vec::new();
    for input in INPUTS {
        let path = root.join(input);
        // relative to the crate dir, so a build tree cloned to another folder keeps cargo's rerun list matching
        println!("cargo:rerun-if-changed={input}");
        files(&path, &mut list);
    }
    let mut material = Vec::new();
    for file in &list {
        let relative = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().replace('\\', "/");
        let bytes = fs::read(file).unwrap_or_default();
        material.extend_from_slice(relative.as_bytes());
        material.push(0);
        material.extend(bytes.iter().copied().filter(|&byte| byte != b'\r'));
        material.push(0);
    }
    let low = fnv(0xcbf2_9ce4_8422_2325, material.iter().copied());
    let high = fnv(0x6c62_272e_07bb_0142, material.iter().copied().rev());
    println!("cargo:rustc-env=REE_CONVERSION_DIGEST={high:016x}{low:016x}");
}
