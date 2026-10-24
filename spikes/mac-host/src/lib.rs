//! Phase 0 spikes for the macOS host (`feat/mac-host`). The examples are
//! throwaway benches and probes. `keymap` and `display_map` are
//! product-quality and move to `host/src/input/` in phase 1.
#![cfg(target_os = "macos")]

pub mod display_map;
pub mod keymap;
