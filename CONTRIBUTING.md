# Contributing to EternalMonitor

Thanks for helping. Bug fixes, tests, docs and new ideas are all welcome. For
anything bigger than a small fix, open an issue or message `aldobenches285` on
Discord first, so we can agree on the approach before you write the code.

## Set up

Follow [Build from source](README.md#build-from-source). You can do most work
on a Mac: the host builds there with a synthetic capture source, and the iPad app
runs in the simulator. Windows-only code (DXGI capture, GPU encoders, input
injection, the virtual display) needs a Windows PC to run for real.

## Make a change

1. Branch from `main` and keep each pull request to one topic.
2. Match the code around you. Comments explain why, not what.
3. Add or update tests with the change. Logic with timers takes an injected
   clock, as the existing tests do.
4. Run these before you push. CI runs the same tests and lints:

   ```bash
   cargo fmt --all --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --locked
   scripts/test_ios.sh             # iPad unit and UI tests, on a Mac
   ```

5. If you changed streaming, repair or input behavior, run
   `scripts/e2e_matrix.sh` too. CI records frame rates on slow hosted machines,
   so the 55 fps gate only runs locally.

## Protocol and shared behavior

The Windows host and the iPad app must agree byte for byte.

- Wire formats live in `proto/` and in `ios/EternalMonitor/Network/WireProtocol.swift`.
  Change both, and add golden vectors in `proto/testdata/` that both test suites read.
- `ios/EternalMonitor/Network/FrameAssembler.swift` defines fragment reassembly and
  repair. `proto/src/reassembly.rs` mirrors it, and `proto/testdata/repair_vectors.txt`
  holds traces that both must pass.
- New messages and fields must be negotiated with capability bits, so older builds
  ignore what they don't know.

## User-facing text

`SETUP.md` quotes the exact labels in both apps. If you rename a button, a setting
or an error message, update `SETUP.md` in the same pull request. Keep release notes
in `RELEASE_NOTES.md` and regenerate the TestFlight notes with
`python3 scripts/testflight_notes.py`.

## Keep private data out

Never commit pairing tokens, pairing codes, QR codes, host logs that contain them,
or screenshots of someone's desktop. The host log records the current pairing code.

## Pull requests

Describe what changed and why, and how you tested it: the commands you ran and,
for hardware changes, the GPU, Windows version and iPad model. Screenshots help for
UI changes.

By contributing, you agree that your work is released under the
[MIT License](LICENSE).
