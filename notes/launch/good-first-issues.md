# Good first issues

Five small, real tasks, ready to paste into a new issue with the
`good first issue` label. Each was checked against `main` (dd9f39a) and
`release/0.3.4-candidate` on 2026-10-31. Check the file paths again before
posting if 0.3.4 has merged since.

---

## Give two iPad close buttons a 44-point tap area

**Context.** Apple's guidelines ask for tap targets of at least 44 × 44 points.
Most of the iPad app's close buttons meet that, and the problem card's close
button is already 44 × 44. Two are drawn at 28 × 28 and are easy to miss with a
finger:

- the close button on the connection error card in
  `ios/EternalMonitor/App/ConnectView.swift` (`.accessibilityLabel("Dismiss error")`)
- the close button on the update notice in
  `ios/EternalMonitor/App/UpdateBanner.swift` (`.accessibilityLabel("Hide update notice")`)

**Files.** `ios/EternalMonitor/App/ConnectView.swift`,
`ios/EternalMonitor/App/UpdateBanner.swift`. The "Dismiss" button on the
problem card in `ConnectView.swift` shows the 44 × 44 version.

**Acceptance criteria.**

- Both buttons respond to taps across a 44 × 44 point area.
- The visible icon stays the same size, and the cards don't grow taller than
  they need to.
- `scripts/test_ios.sh` passes. If a UI test taps either button, it still does.

---

## Document `ETERNAL_SYNTH_SIZE` and `ETERNAL_FORCE_IDR_PERIOD`

**Context.** README.md's "Host environment variables" table lists the knobs
contributors use when testing the host. Two that the test scripts rely on are
missing:

- `ETERNAL_SYNTH_SIZE=WxH` sets the synthetic capture size, from 16 × 16 up to
  7680 × 4320; the default is 1280 × 720 (`host/src/capture/synthetic.rs`).
- `ETERNAL_FORCE_IDR_PERIOD=N` makes the encoder send a keyframe every N frames
  (`host/src/encoder/mod.rs`). `scripts/e2e_matrix.sh`, `scripts/e2e_device.py`
  and `scripts/e2e_real.py` set it to test keyframe bursts.

**Files.** `README.md` (the table inside "Host environment variables"),
`host/src/capture/synthetic.rs`, `host/src/encoder/mod.rs`.

**Acceptance criteria.**

- Two new rows in the table, worded like the existing ones, with
  `ETERNAL_FORCE_IDR_PERIOD` marked "Test only" like `ETERNAL_DROP`.
- The defaults and limits match the code.

---

## Bring ARCHITECTURE.md's opening line up to date

**Context.** ARCHITECTURE.md opens with "v0.3.0 keeps protocol v2." The current
release is 0.3.3 and still uses protocol v2, so the line reads as if the
document stopped at 0.3.0.

**Files.** `ARCHITECTURE.md`. `RELEASE_NOTES.md` shows the current version.

**Acceptance criteria.**

- The opening says the current releases use protocol v2, without naming a
  version that will go stale at the next release.
- The rest of the paragraph and its link to `docs/hardware-verification.md`
  stay as they are.

---

## Check relative links in the Markdown files in CI

**Context.** The README exists in 11 languages (English plus `i18n/README.*.md`), and the docs
link to each other with relative paths. All of those links resolve today. A file
rename could break them without anyone noticing, because CI doesn't check them.

**Files.** A new `scripts/check_links.py` with `scripts/test_check_links.py`
(stdlib only, like the other scripts), and `.github/workflows/ci.yml`, which
already runs `python3 -m unittest discover -s scripts -p 'test_*.py'`.

**Acceptance criteria.**

- The script reads every tracked `*.md` file, finds links like
  `[text](path)` and `[text](path#anchor)` that aren't URLs, and fails with
  the file and link when the target file doesn't exist.
- Checking `#anchor` headings is out of scope.
- A unit test covers a good link, a broken link and an external URL, using
  temporary files.
- CI runs the check, and it passes on `main`.

---

## Let growth_report.py save each run for later comparison

**Context.** `scripts/growth_report.py` prints stars, downloads and traffic.
GitHub keeps traffic for 14 days only, so comparing a launch week with a month
later means saving each run somewhere. Today that takes a shell redirect, and
nothing compares two runs.

**Files.** `scripts/growth_report.py`, `scripts/test_growth_report.py`.

**Acceptance criteria.**

- `--save DIR` writes the JSON report to `DIR/growth-YYYY-MM-DD.json` and still
  prints the text report.
- `--since FILE` adds the change since an earlier saved report to the stars,
  downloads and forks lines, for example `Stars 210 (+24 since 2026-10-31)`.
- The tests use the recorded responses already in `test_growth_report.py` and
  make no network calls.
