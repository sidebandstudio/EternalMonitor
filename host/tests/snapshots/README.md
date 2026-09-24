# Host GUI references

`gui_snapshots.rs` drives the real `AnalyzerApp` through AccessKit with fixed pipeline statistics and a simulated USB session. It checks stream measurements, pairing code rotation, QR opening, every navigation item, FPS and input settings, cached update notification and dismissal, and hardware fallback guidance. WGPU renders these PNGs on macOS.

The magenta rectangle covers only the six random pairing digits. The test checks that the displayed value matches the host's actual code before masking it. Pairing tokens, network address, measurements and elapsed time use fixed fixture values. No real settings, network requests or desktop inputs are involved.

Run `PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig cargo test -p eternal-host --test gui_snapshots`. To accept an intentional visual change, prefix the command with `UPDATE_SNAPSHOTS=true`, inspect each changed image, then rerun without that variable. CI uploads the references and any `.new.png` or `.diff.png` images. The default per-pixel tolerance applies with zero allowed failed pixels.
