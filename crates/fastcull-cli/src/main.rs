//! Headless driver for the FastCull engine.
//!
//! Grows one subcommand per engine capability (scan, thumbs, cull, copy) as
//! milestones land; integration tests run the engine through this binary.

fn main() {
    println!(
        "fastcull-cli {} — subcommands arrive with milestone M1",
        fastcull_core::VERSION
    );
}
