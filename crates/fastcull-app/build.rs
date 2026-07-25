fn main() {
    // The Slint compiler is deeply recursive and build scripts are compiled
    // unoptimized; Windows gives the main thread 1 MB of stack (vs 8 MB on
    // Linux), which main.slint's growth overflowed (CI: STATUS_STACK_OVERFLOW
    // 0xc00000fd on every Windows run since M3). Standard workaround: compile
    // on a thread with an explicit, roomy stack.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| slint_build::compile("ui/main.slint"))
        .expect("spawning slint-build thread")
        .join()
        .expect("slint-build thread panicked")
        .expect("compiling ui/main.slint");
}
