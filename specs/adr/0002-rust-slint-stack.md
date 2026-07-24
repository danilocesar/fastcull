# ADR 0002: Rust core + Slint UI

**Status**: accepted (2026-07-24, user decision)

## Context

Benchmarks (ADR 0001) showed the Rust decode path (rawler + zune-jpeg/turbojpeg)
equals the C++ path (LibRaw + libjpeg-turbo), so the stack choice was about UI,
build simplicity, and AI-agent development ergonomics. Candidates: Rust+egui,
Rust+Slint, Rust+Tauri/React, C++/Qt6.

## Decision

- **Rust** everywhere: one toolchain on Linux+Windows (`cargo build/test/clippy`),
  memory safety as a compile-time reviewer for agent-generated code, static
  binaries, GPL-3.0-compatible ecosystem (rawler MPL/LGPL-class, slint GPLv3
  option).
- **Slint** for the UI (user choice; user is a former Qt developer): QML-like
  declarative `.slint` markup from the original QML creators, compiled, GPU
  rendered. Default **FemtoVG/OpenGL renderer** (pure-Rust-friendly builds); Skia
  optional behind a feature flag.
- Tauri rejected: webview (WebKitGTK on Linux) is a permanent performance ceiling
  and double-buffers every thumbnail across IPC. C++/Qt rejected: dual-platform
  CMake/vcpkg cost + memory-safety burden for agent-written code.

## Consequences

- Grid virtualization needs the windowed-model pattern (Slint has no virtualized
  GridView) — flagged as the M2 prototype risk in ui-grid spec.
- Contributors need only rustup; CI is a 2-OS cargo matrix.
