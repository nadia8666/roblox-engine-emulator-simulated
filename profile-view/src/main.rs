//! `ree-profile-view view|render ...`: the profile viewer's binary, which `ree profile view|render` runs (lib.rs)

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(ree_profile_view::main("ree profile", &args));
}
