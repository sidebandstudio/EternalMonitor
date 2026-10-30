# Settings in the update manifest

`latest.json` (https://eternalmonitor.dev/latest.json) can change a few
behaviours without a release. One rule decides what may go here: **remote
config never blocks or degrades a PC or iPad that works.** It may only

- stage the soft update prompt over a few days,
- lengthen a timeout, up to a bound, but never shorten it below today's
  default (a shorter wait would fail PCs and iPads that are slow but work),
- start Second screen sessions in the mirror on narrowly named hardware
  where the second screen is known not to start, which the user can always
  override with Try again,
- choose how the PC's window draws (DX12 or Vulkan, on its own graphics
  card) on a named graphics vendor and driver range, from the next launch,
  below the user's own choice and the PC's memory of renderers that crashed.

It has no kill switch, no minimum version, no forced update or downgrade,
and nothing that turns a feature off for everyone. Every field is optional.
An app that finds a field missing, malformed, out of range or from a newer
config schema uses its built-in default for it and says so in one log line
per launch: `remote_config=applied …` or `remote_config=defaults reason=…`
(`missing`, `invalid`, `schema`, `unreadable`, `stale`). No ids or names are
logged.

Apps from before these fields ignore them: both parsers skip unknown keys
(tested against a copy of the 0.3.3 parsers and
`proto/testdata/latest_remote_config.json`). `scripts/check-versions.sh`
and `scripts/latest_json.py` (`--rollout-start`/`--rollout-days`,
`--windows-config`, `--ipad-config`) check every field with the same code,
`scripts/manifest_fields.py`, so a manifest with a bad value can't be
published. The
top-level `schema` stays 1; a newer one would stop old apps' update checks.

## Fields

| Field | Type | Range | Default (missing or bad) |
| --- | --- | --- | --- |
| `windows.note`, `ipad.note` | string | 1–200 characters, no control, format or line/paragraph separator characters (Unicode Cc, Cf, Zl, Zp) | no note |
| `windows.rollout`, `ipad.rollout` | `{ "start": "YYYY-MM-DDTHH:MM:SSZ", "days": N }` | `days` 0–14 | everyone at once |
| `windows.config.schema`, `ipad.config.schema` | integer | 1 | whole `config` ignored |
| `windows.config.attach_timeout_s` | integer seconds | 30–60 | 30 |
| `windows.config.second_screen_rules` | list of rules | at most 8 | no rules |
| `windows.config.renderer_rules` | list of rules | at most 8 | no rules |
| `ipad.config.first_frame_timeout_s` | integer seconds | 10–20 | 10 |

### Staged update prompt

Each install takes a fixed place in the ramp from the first 8 bytes of
SHA-256(`"{version}/{id}"`), so a different set goes first each release.
The PC's id is a random value kept in `update-state.json` that is never
sent. The iPad reuses its diagnostics installation id, which diagnostics
uploads carry; the rollout only hashes it on the iPad and sends neither the
id nor the hash. An install
whose place is a share *p* of the ramp sees the prompt at
`start + p × days`; before `start` nobody does, and from `start + days`
everybody. `check-versions.sh` refuses a `start` more than 14 days ahead;
an app that still sees one (a clock set back) shows the prompt at once.
Once an install's prompt has shown for a release, it keeps showing for that
release, whatever the clock does later. A rollout saved with an earlier
check is held to the same limits when it is read back.

Only the automatic prompt waits: the PC's banner, popup and notification,
and the iPad's banner and popup. Check now, Update now, the PC's
version-mismatch banner, Settings on both apps and the download page always
offer the newest version.

### Attach and first-picture timeouts

`attach_timeout_s` is how long the PC waits for the virtual display to
attach after its driver starts (`ETERNAL_VDD_TIMEOUT_SECS` still wins).
`first_frame_timeout_s` is how long the iPad waits for the first picture
after connecting. Both apply from the app's next launch. Each may only be
raised: a value below today's default (30 s, 10 s) is out of range and
ignored, since a shorter attach wait would fail PCs that attach late and #108
would then remember that failure, and a shorter first-picture wait would cut
off slow connections and could stop the iPad's automatic reconnects.

### Second screen rules

```json
{
  "gpus": ["intel", "nvidia"],
  "gpu_driver": { "gpu": "nvidia", "min": "32.0.15.6000", "max": "32.0.15.6099" },
  "display_driver": { "min": "25.7.0", "max": "25.7.99" },
  "windows": { "min": "26100.0", "max": "26100.2999" }
}
```

- `gpus` is the exact set of graphics vendors the PC has, from `amd`,
  `intel`, `nvidia` and `qualcomm`. Windows' software renderer doesn't
  count; a PC with any other graphics adapter matches no rule.
- `gpu_driver` is required: one graphics driver release of a vendor in
  `gpus`, which every card of that vendor must have. Both ends have four
  numbers, equal in the first three, and the last differs by at most 99.
  NVIDIA's last number is its release (32.0.15.6094 is 560.94), so this
  is one release branch such as 560.xx; for Intel and AMD it is a handful
  of builds. So no rule matches every PC of a vendor.
- At least one of `display_driver` and `windows` is required as well.
  `display_driver` is an exact virtual display driver version or a patch
  range: two to four numbers, equal but for the last (25.7.0 to 25.7.99).
  `windows` is one Windows build with a range of its updates (26100.0 to
  26100.2999, build.UBR). Combined with the driver release, no rule
  matches every PC on a Windows release.
- Ranges are inclusive, `min` ≤ `max`, and an unknown version never matches.
- A rule with a key the app doesn't know is ignored, so a newer constraint
  can't widen it on an older app. More than 8 rules: none apply. The PC's
  parser (`host/src/remote_config.rs`) and `check-versions.sh` apply the
  same limits.

On a matching PC in Second screen mode, the first connection of an app run
saves a `hardware_rule` failure without turning the driver on, so it is
handled like a failure the PC remembers: the iPad gets the mirror, the
Stream page shows the notice with Try again, and the host tries by itself
after the usual waits (1 h, then 6 h, …).

A PC where the second screen has shown a picture, with or without a rule
then, or where Try again was clicked, is exempt from every rule for good
(`second-screen-rule.json`, keyed by its graphics cards, so a driver or
Windows update keeps it): a rule never moves a PC that works into the
mirror. Mirror mode is never affected, and Second screen is never hidden.
When the manifest drops the rule, or the PC is exempt, a saved
`hardware_rule` failure is forgotten at the next launch, in that app run
even if the state file can't be cleared yet.

### Window renderer rules

A driver that crashes the PC's window at start can be steered to another
backend without a release. Example: Intel driver 31.0.101.x crashes in
Vulkan, so draw with DX12 there:

```json
"renderer_rules": [
  {
    "vendor": "intel",
    "driver": { "min": "31.0.101.0", "max": "31.0.101.65535" },
    "backend": "dx12"
  }
]
```

- `vendor` is `amd`, `intel` or `nvidia`.
- `driver` is optional: a Windows driver version range with inclusive
  bounds, `min`, `max` or both. Each bound has four numbers, and `min` ≤
  `max`. Without `driver`, the rule names every driver of that vendor. The
  PC compares the bounds with the driver version DXGI reports for the card,
  which is how its renderer crash memory keys drivers. A card whose driver
  version isn't known only matches a rule without a range.
- `backend` is `dx12` or `vulkan`, on the card the window would draw on.
  Never `warp`: Windows' software renderer would draw a working PC slowly,
  and a slow window still starts, so the PC's crash memory would never set
  it aside. A `warp` rule is ignored, and `check-versions.sh` refuses it.
  WARP stays the PC's own last fallback and a Troubleshooting choice.
- Driver versions use ASCII digits `0`–`9` only, as the PC reads them.
- A rule with a key or value the app doesn't know is ignored, so a newer
  constraint can't widen it on an older app. More than 8 rules, or
  `renderer_rules` that isn't a list: none apply. As with
  `second_screen_rules`, `check-versions.sh` refuses the whole manifest for
  any bad rule.

A rule applies **at the next launch only**: the window's renderer is chosen
when it starts, from the last manifest the update check saved, and a running
window never changes renderer. The rule is matched against the graphics card
the window would draw on anyway (the built-in choice decides, for example
the NVIDIA card on a laptop that also has an Intel or AMD iGPU), and the
first rule that names its vendor and driver applies.

What decides the window's backend, highest first:

1. the `WGPU_BACKEND` environment variable;
2. Settings > Troubleshooting > Window drawing, when it isn't Automatic.
   A chosen backend that already failed to start the window is set aside,
   as before, and the built-in choice draws, without any rule: the user
   chose not to leave the window to the PC;
3. the PC's renderer crash memory: a rule never draws with a backend that
   failed on that card and driver before, even one the PC would retry
   after some clean launches;
4. a matching remote rule;
5. the built-in choice.

A rule can only choose among the backends the PC lists for that card. It is
ignored when its backend isn't listed (for example after the PC narrowed
itself to DX12), when the backend lacks the features the window needs
(Intel's old Vulkan drivers, issue #59), on a PC with two cards of the same
model, or when the PC can't remember a crash under it: its start marker
couldn't be saved, or its renderer history can't be (`no_marker`). Each
launch logs one line when a rule names the card: `renderer_rule
applied=intel:31.0.101.2111->dx12`, or `renderer_rule ignored
reason=env|choice|set_aside|unavailable|incapable|twin_gpus|no_marker
rule=…`. Nothing else about the card is logged.

**A bad rule costs at most one failed start per driver version.** The
launch it steers leaves the same start marker as any other, and a rule only
draws once that marker is saved and the renderer history saves too. If the
window never appears, the next launch records that backend as failed on
that card and driver, ignores the rule (`reason=set_aside`) and draws as
before. A launch that can't save the history keeps the marker, so a later
one counts the crash. The record stays until that backend draws on that driver, the user
resets crash avoidance in Troubleshooting, or the driver changes. A new
driver the rule still names gets one try of its own.

The PC saves the manifest's values in `remote-config.json` after each check
and applies them at the next launch for up to 14 days; with automatic
checks off, or offline for longer, it returns to the defaults.
