//! Phase 0 spikes for the macOS host (`feat/mac-host`). The examples are
//! throwaway benches and probes. `keymap` and `display_map` moved to the
//! host in phase 1 (`host/src/input/mac/`); they are included from there so
//! the spike examples keep building against the same code.
#![cfg(target_os = "macos")]

#[path = "../../../host/src/input/mac/display.rs"]
pub mod display_map;
#[path = "../../../host/src/input/mac/keys.rs"]
pub mod keymap;
