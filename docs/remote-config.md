# Settings in the update manifest

`latest.json` (https://eternalmonitor.dev/latest.json) can change a few
behaviours without a release. One rule decides what may go here: **remote
config never blocks or degrades a PC or iPad that works.** It may only

- stage the soft update prompt over a few days,
- lengthen a timeout, up to a bound, but never shorten it below today's
  default (a shorter wait would fail PCs and iPads that are slow but work),
- start Second screen sessions in the mirror on narrowly named hardware
  where the second screen is known not to start, which the user can always
  override with Try again.

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
refuses a manifest with a bad value, so one can't be published. The
top-level `schema` stays 1; a newer one would stop old apps' update checks.

## Fields

| Field | Type | Range | Default (missing or bad) |
| --- | --- | --- | --- |
| `windows.note`, `ipad.note` | string | 1–200 characters, no control, format or line/paragraph separator characters (Unicode Cc, Cf, Zl, Zp) | no note |
| `windows.rollout`, `ipad.rollout` | `{ "start": "YYYY-MM-DDTHH:MM:SSZ", "days": N }` | `days` 0–14 | everyone at once |
| `windows.config.schema`, `ipad.config.schema` | integer | 1 | whole `config` ignored |
| `windows.config.attach_timeout_s` | integer seconds | 30–60 | 30 |
| `windows.config.second_screen_rules` | list of rules | at most 8 | no rules |
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

The PC saves the manifest's values in `remote-config.json` after each check
and applies them at the next launch for up to 14 days; with automatic
checks off, or offline for longer, it returns to the defaults.
