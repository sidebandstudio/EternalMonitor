# Host GUI references

`gui_snapshots.rs` drives the real `AnalyzerApp` through AccessKit with fixed pipeline statistics and a simulated USB session. It checks the streaming header and connected-iPad measurements, the Pairing, USB connection and PC audio rows, pairing code rotation, the QR dialog, every navigation item, frame rate and encoder input settings with the restart prompt they raise, the cached update notice and its dismissal, and hardware fallback guidance. WGPU renders these PNGs on macOS.

The magenta rectangle covers only the six random pairing digits. The test checks that the displayed value matches the host's actual code before masking it. Pairing tokens, network address, measurements and elapsed time use fixed fixture values. No real settings, network requests or desktop inputs are involved.

Run `PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig cargo test -p eternal-host --test gui_snapshots`. To accept an intentional visual change, prefix the command with `UPDATE_SNAPSHOTS=true`, inspect each changed image, then rerun without that variable. CI uploads the references and any `.new.png` or `.diff.png` images. The default per-pixel tolerance applies with zero allowed failed pixels.

For design review, `EM_UI_REVIEW_DIR=/tmp/review cargo test -p eternal-host --test gui_snapshots -- --ignored` renders every page at the default window size, the minimum size and 2x scale, plus the website's product shot, into that directory. Those images are not references and are never compared.
