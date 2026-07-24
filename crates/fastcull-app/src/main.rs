//! FastCull desktop application. The Slint UI shell lands with milestone M2;
//! this crate stays a thin bridge between `fastcull-core` state and Slint
//! models (see specs/modules/ui-grid.md).

fn main() {
    println!(
        "fastcull {} — UI shell arrives with milestone M2",
        fastcull_core::VERSION
    );
}
