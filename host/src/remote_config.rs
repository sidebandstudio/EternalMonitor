//! Behaviour the update manifest may change without a release.
//!
//! The rule: remote config never blocks or degrades a PC that works. It holds
//! only the staging of the soft update prompt, a bounded attach wait,
//! narrow hardware rules that start Second screen sessions in the mirror on
//! PCs where the second screen is known not to start, which Try again always
//! overrides, and rules that pick how the window draws on named graphics
//! drivers from the next launch, below the user's choice and the PC's own
//! crash memory. There is no kill switch, minimum version or forced update.
//! Anything missing, unreadable, too old, from a newer schema or out of range
//! means the built-in defaults, with one log line per app run.
//!
//! The update worker saves the manifest's `windows.rollout` and
//! `windows.config` after each check ([`save_fetched`]); the next app run
//! applies them ([`load`]). The schema is in `docs/remote-config.md`.

use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only `config.schema` this app reads. A newer one means defaults.
pub const SCHEMA: u64 = 1;
/// A staged prompt reaches every install within this many days of its start.
pub const MAX_ROLLOUT_DAYS: u64 = 14;
/// The virtual display attach wait the manifest may set: only ever longer
/// than today's 30 s, since a shorter one would fail PCs that attach late.
pub const ATTACH_TIMEOUT_S: RangeInclusive<u64> = 30..=60;
/// More rules than this is a mistake: none of them apply.
pub const MAX_RULES: usize = 8;
/// How far the last number of a rule's graphics driver range may span. An
/// NVIDIA version's last number is its release (6094 is 560.94), so this is
/// one release branch; for Intel and AMD it is a handful of builds.
pub const GPU_DRIVER_SPAN: u32 = 99;
/// The vendors a rule may name. A PC with any other graphics card matches no rule.
pub const VENDORS: [&str; 4] = ["amd", "intel", "nvidia", "qualcomm"];
/// The vendors a renderer rule may name, with their PCI vendor ids.
const RENDERER_VENDORS: [(&str, u32); 3] = [("amd", 0x1002), ("intel", 0x8086), ("nvidia", 0x10de)];

const DAY: u64 = 24 * 60 * 60;
/// A saved config is applied this long after the check that fetched it. A PC
/// that stops checking (offline, or automatic checks turned off) goes back
/// to the defaults.
const MAX_AGE: u64 = 14 * DAY;
const FILE_NAME: &str = "remote-config.json";
const FORMAT: u32 = 1;

// ── Staged update prompt ────────────────────────────────────────────────────

/// `rollout: { "start": "2026-11-02T00:00:00Z", "days": 7 }`: installs see
/// the soft update prompt spread evenly over `days` from `start`. Only the
/// prompt waits; a manual check, Update now and the website always offer the
/// latest version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollout {
    /// Seconds since 1970.
    pub start: u64,
    pub days: u64,
}

impl Rollout {
    /// `None` for anything malformed or out of range: everyone sees the
    /// prompt at once, as before rollouts existed.
    pub fn parse(value: &Value) -> Option<Self> {
        let start = parse_utc(value.get("start")?.as_str()?)?;
        let days = value.get("days")?.as_u64()?;
        Self { start, days }.valid()
    }

    fn valid(self) -> Option<Self> {
        (self.days <= MAX_ROLLOUT_DAYS).then_some(self)
    }

    /// A rollout saved in `update-state.json`, held to the same limits as a
    /// fresh one: anything else is no rollout, and the rest of the file
    /// still loads.
    pub fn saved<'de, D: serde::Deserializer<'de>>(data: D) -> Result<Option<Self>, D::Error> {
        let value = Option::<Value>::deserialize(data)?;
        Ok(value
            .and_then(|value| serde_json::from_value::<Self>(value).ok())
            .and_then(Self::valid))
    }

    /// Whether install `id` may see the prompt for `version` at `now`. Each
    /// install waits a share of the ramp fixed by a hash of its id and the
    /// version, so a different set of installs goes first each release.
    /// Once it shows, the caller keeps showing it for that version
    /// (`check-versions.sh` refuses such starts, but a clock set
    /// back could also move them).
    pub fn shows(&self, id: &str, version: &str, now: u64) -> bool {
        // A start further ahead than any ramp lasts is a typo, not a plan,
        // and a ramp longer than allowed is no ramp.
        if self.days > MAX_ROLLOUT_DAYS || self.start > now.saturating_add(MAX_ROLLOUT_DAYS * DAY) {
            return true;
        }
        let ramp = u128::from(self.days.saturating_mul(DAY));
        let wait = ((u128::from(bucket(id, version)) * ramp) >> 64) as u64;
        now >= self.start.saturating_add(wait)
    }

    /// For the log: `7d`.
    fn token(this: Option<Self>) -> String {
        this.map_or_else(|| "none".to_string(), |r| format!("{}d", r.days))
    }
}

/// Where in the ramp an install falls: the first 8 bytes of
/// SHA-256("{version}/{id}"), uniform over `u64`. The iPad computes the same.
pub fn bucket(id: &str, version: &str) -> u64 {
    let digest = ring::digest::digest(&ring::digest::SHA256, format!("{version}/{id}").as_bytes());
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest.as_ref()[..8]);
    u64::from_be_bytes(first)
}

/// `YYYY-MM-DDTHH:MM:SSZ` in seconds since 1970. Other RFC 3339 forms are
/// refused, as `check-versions.sh` refuses them.
pub fn parse_utc(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    let shape = b.len() == 20
        && [
            (4, b'-'),
            (7, b'-'),
            (10, b'T'),
            (13, b':'),
            (16, b':'),
            (19, b'Z'),
        ]
        .iter()
        .all(|&(i, c)| b[i] == c);
    if !shape {
        return None;
    }
    let field = |at: usize, len: usize| {
        let digits = &b[at..at + len];
        digits
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| text[at..at + len].parse::<u64>().ok())
            .flatten()
    };
    let (year, month, day) = (field(0, 4)?, field(5, 2)?, field(8, 2)?);
    let (hour, minute, second) = (field(11, 2)?, field(14, 2)?, field(17, 2)?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if year < 1970
        || !(1..=12).contains(&month)
        || day == 0
        || day > month_days[month as usize - 1]
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    // Days from 1970-01-01 (Howard Hinnant's days_from_civil).
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * DAY + hour * 3600 + minute * 60 + second)
}

// ── Per-PC defaults ─────────────────────────────────────────────────────────

/// `windows.config`, validated. Each value that is out of range or malformed
/// is left at its default on its own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub attach_timeout_s: Option<u64>,
    pub rules: Vec<Rule>,
    pub renderer_rules: Vec<RendererRule>,
}

/// `Err(reason)` when the whole object is unusable; the reason is a log token.
pub fn parse_config(value: Option<&Value>) -> Result<Config, &'static str> {
    let object = match value {
        None | Some(Value::Null) => return Err("missing"),
        Some(Value::Object(object)) => object,
        Some(_) => return Err("invalid"),
    };
    match object.get("schema").and_then(Value::as_u64) {
        Some(SCHEMA) => {}
        Some(schema) if schema > SCHEMA => return Err("schema"),
        _ => return Err("invalid"),
    }
    let attach_timeout_s = object
        .get("attach_timeout_s")
        .and_then(Value::as_u64)
        .filter(|seconds| ATTACH_TIMEOUT_S.contains(seconds));
    let rules = match object.get("second_screen_rules") {
        Some(Value::Array(rules)) if rules.len() <= MAX_RULES => {
            rules.iter().filter_map(Rule::parse).collect()
        }
        _ => Vec::new(),
    };
    let renderer_rules = match object.get("renderer_rules") {
        Some(Value::Array(rules)) if rules.len() <= MAX_RULES => {
            rules.iter().filter_map(RendererRule::parse).collect()
        }
        _ => Vec::new(),
    };
    Ok(Config {
        attach_timeout_s,
        rules,
        renderer_rules,
    })
}

/// A set of PCs where the second screen is known not to start: Second
/// screen sessions there start in the mirror, as after a failure the PC
/// remembers, and Try again overrides it for good. A rule must name the
/// exact set of graphics vendors, one graphics driver release
/// ([`GPU_DRIVER_SPAN`]), and an exact virtual display driver version or
/// patch range, an exact Windows build (any update of it), or both. So no
/// rule matches every PC of a vendor or every PC on a Windows release.
/// Anything broader, or a key this app doesn't know, and the rule is
/// ignored: a rule can only ever be narrower than intended.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    gpus: Vec<String>,
    gpu_driver: GpuDriver,
    #[serde(default)]
    display_driver: Option<Range>,
    #[serde(default)]
    windows: Option<Range>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct GpuDriver {
    gpu: String,
    min: String,
    max: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Range {
    min: String,
    max: String,
}

impl Rule {
    pub fn parse(value: &Value) -> Option<Self> {
        let rule: Rule = serde_json::from_value(value.clone()).ok()?;
        let mut gpus = rule.gpus.clone();
        gpus.sort();
        gpus.dedup();
        let vendors_ok = !gpus.is_empty()
            && gpus.len() == rule.gpus.len()
            && gpus.iter().all(|gpu| VENDORS.contains(&gpu.as_str()));
        let driver = &rule.gpu_driver;
        let driver_ok = rule.gpus.contains(&driver.gpu)
            && narrow(&driver.min, &driver.max, 4..=4, GPU_DRIVER_SPAN);
        let display_ok = rule
            .display_driver
            .as_ref()
            .is_none_or(|r| narrow(&r.min, &r.max, 2..=4, u32::MAX));
        let windows_ok = rule
            .windows
            .as_ref()
            .is_none_or(|r| narrow(&r.min, &r.max, 2..=2, u32::MAX));
        let constrained = rule.display_driver.is_some() || rule.windows.is_some();
        (vendors_ok && driver_ok && display_ok && windows_ok && constrained)
            .then_some(Rule { gpus, ..rule })
    }

    /// `graphics` holds each card's name and driver version, as the second
    /// screen's fingerprint does; an unknown version never matches.
    pub fn matches(&self, graphics: &[String], display_driver: &str, windows: &str) -> bool {
        let cards: Vec<(&str, &str)> = graphics.iter().filter_map(|g| card(g)).collect();
        let mut vendors: Vec<&str> = cards.iter().map(|(vendor, _)| *vendor).collect();
        vendors.sort();
        vendors.dedup();
        if vendors != self.gpus {
            return false;
        }
        let range = &self.gpu_driver;
        let driver_ok = cards
            .iter()
            .filter(|(vendor, _)| *vendor == range.gpu)
            .all(|(_, version)| within(version, &range.min, &range.max));
        driver_ok
            && self
                .display_driver
                .as_ref()
                .is_none_or(|r| within(display_driver, &r.min, &r.max))
            && self
                .windows
                .as_ref()
                .is_none_or(|r| within(windows, &r.min, &r.max))
    }
}

/// `(vendor, driver version)` from "NVIDIA GeForce RTX 3050 Laptop GPU
/// 32.0.15.6094". Windows' software renderer is no card; any other name is
/// `other`, which no rule names.
fn card(graphics: &str) -> Option<(&'static str, &str)> {
    let (name, driver) = graphics.trim().rsplit_once(' ')?;
    let lower = name.to_ascii_lowercase();
    if lower.contains("microsoft basic render") {
        return None;
    }
    let words: Vec<&str> = lower.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    let has = |word: &str| words.contains(&word);
    let vendor = if has("nvidia") {
        "nvidia"
    } else if has("intel") {
        "intel"
    } else if has("amd") || has("radeon") {
        "amd"
    } else if has("qualcomm") || has("adreno") {
        "qualcomm"
    } else {
        "other"
    };
    Some((vendor, driver))
}

/// The numbers of a dotted version of one to four numbers.
fn numbers(text: &str) -> Option<Vec<u32>> {
    let parts = text
        .split('.')
        .map(|piece| {
            (!piece.is_empty() && piece.bytes().all(|b| b.is_ascii_digit()))
                .then(|| piece.parse().ok())
                .flatten()
        })
        .collect::<Option<Vec<u32>>>()?;
    (parts.len() <= 4).then_some(parts)
}

/// For comparing, padded with zeros.
fn version(text: &str) -> Option<[u32; 4]> {
    let mut padded = [0u32; 4];
    for (slot, number) in padded.iter_mut().zip(numbers(text)?) {
        *slot = number;
    }
    Some(padded)
}

/// A range a rule may use: both ends `parts` numbers long and equal but for
/// the last number, which rises by at most `span`.
fn narrow(min: &str, max: &str, parts: RangeInclusive<usize>, span: u32) -> bool {
    let (Some(lo), Some(hi)) = (numbers(min), numbers(max)) else {
        return false;
    };
    let last = lo.len() - 1;
    lo.len() == hi.len()
        && parts.contains(&lo.len())
        && lo[..last] == hi[..last]
        && lo[last] <= hi[last]
        && hi[last] - lo[last] <= span
}

fn within(value: &str, min: &str, max: &str) -> bool {
    match (version(value), version(min), version(max)) {
        (Some(value), Some(lo), Some(hi)) => lo <= value && value <= hi,
        _ => false,
    }
}

// ── Window renderer ─────────────────────────────────────────────────────────

/// `{ "vendor": "intel", "driver": { "min": "31.0.101.0", "max":
/// "31.0.101.65535" }, "backend": "dx12" }`: the window draws with `backend`
/// from the next launch when the graphics card it would draw on is of
/// `vendor`, with a driver in the inclusive range (either end may be left
/// out, not both; no `driver` is every driver). The user's Window drawing
/// choice, `WGPU_BACKEND` and the renderer's crash memory all win over it
/// (`gui/renderer.rs`). A key or value this app doesn't know, and the rule
/// is ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RendererRule {
    vendor: &'static str,
    /// PCI vendor id.
    vendor_id: u32,
    min: Option<[u32; 4]>,
    max: Option<[u32; 4]>,
    pub backend: RendererBackend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RendererBackend {
    Dx12,
    Vulkan,
    /// Microsoft's software renderer, on DX12.
    Warp,
}

impl RendererBackend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Dx12 => "dx12",
            Self::Vulkan => "vulkan",
            Self::Warp => "warp",
        }
    }
}

impl RendererRule {
    pub fn parse(value: &Value) -> Option<Self> {
        let rule = value.as_object()?;
        if !rule
            .keys()
            .all(|key| ["vendor", "driver", "backend"].contains(&key.as_str()))
        {
            return None;
        }
        let vendor = rule.get("vendor")?.as_str()?;
        let (vendor, vendor_id) = RENDERER_VENDORS
            .into_iter()
            .find(|(name, _)| *name == vendor)?;
        let backend = rule.get("backend")?.as_str()?;
        let backend = [
            RendererBackend::Dx12,
            RendererBackend::Vulkan,
            RendererBackend::Warp,
        ]
        .into_iter()
        .find(|b| b.name() == backend)?;
        let (min, max) = match rule.get("driver") {
            None => (None, None),
            Some(driver) => {
                let driver = driver.as_object()?;
                if driver.is_empty() || !driver.keys().all(|key| key == "min" || key == "max") {
                    return None;
                }
                // An end that is there must be a driver version, not null.
                let end = |key: &str| match driver.get(key) {
                    None => Some(None),
                    Some(end) => Some(Some(driver_version(end.as_str()?)?)),
                };
                let (min, max) = (end("min")?, end("max")?);
                if min.zip(max).is_some_and(|(min, max)| min > max) {
                    return None;
                }
                (min, max)
            }
        };
        Some(Self {
            vendor,
            vendor_id,
            min,
            max,
            backend,
        })
    }

    /// Whether the rule names a graphics card of PCI `vendor` with `driver`,
    /// as the renderer's crash memory keys it. A driver that is no version
    /// matches only a rule without a range.
    pub fn matches(&self, vendor: u32, driver: &str) -> bool {
        if vendor != self.vendor_id {
            return false;
        }
        if self.min.is_none() && self.max.is_none() {
            return true;
        }
        driver_version(driver).is_some_and(|driver| {
            self.min.is_none_or(|min| min <= driver) && self.max.is_none_or(|max| driver <= max)
        })
    }

    /// For the log: `intel`.
    pub fn vendor(&self) -> &'static str {
        self.vendor
    }
}

/// A Windows driver version: four numbers, as `31.0.101.2111`.
fn driver_version(text: &str) -> Option<[u32; 4]> {
    numbers(text)?.try_into().ok()
}

// ── Saved between app runs ──────────────────────────────────────────────────

/// `remote-config.json` beside the settings: the manifest's raw values, so
/// each app run validates them with its own rules.
#[derive(Debug, Serialize, Deserialize)]
struct Saved {
    format: u32,
    fetched_at: u64,
    #[serde(default)]
    rollout: Value,
    #[serde(default)]
    config: Value,
}

/// What this app run applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub rollout: Option<Rollout>,
    /// `Err(reason)`: every value is its default.
    pub config: Result<Config, &'static str>,
}

impl Applied {
    fn defaults(reason: &'static str) -> Self {
        Self {
            rollout: None,
            config: Err(reason),
        }
    }

    pub fn attach_timeout(&self) -> Option<Duration> {
        let seconds = self.config.as_ref().ok()?.attach_timeout_s?;
        Some(Duration::from_secs(seconds))
    }

    /// The renderer rules, for the next window to start.
    pub fn renderer_rules(&self) -> &[RendererRule] {
        self.config
            .as_ref()
            .map_or(&[], |config| &config.renderer_rules)
    }

    /// Whether a rule names this PC.
    pub fn second_screen_rule(
        &self,
        graphics: &[String],
        display_driver: &str,
        windows: &str,
    ) -> bool {
        self.config.as_ref().is_ok_and(|config| {
            config
                .rules
                .iter()
                .any(|rule| rule.matches(graphics, display_driver, windows))
        })
    }

    /// The one launch line. Diagnostics count `remote_config=`.
    pub fn summary(&self, ss_rule: Option<bool>) -> String {
        let rollout = Rollout::token(self.rollout);
        match &self.config {
            Err(reason) => format!("remote_config=defaults reason={reason} rollout={rollout}"),
            Ok(config) => format!(
                "remote_config=applied rollout={rollout} ss_rule={} attach_s={}",
                match ss_rule {
                    Some(true) => "match",
                    Some(false) => "none",
                    None => "n/a",
                },
                config
                    .attach_timeout_s
                    .map_or_else(|| "default".to_string(), |s| s.to_string()),
            ),
        }
    }
}

/// The values saved at `path`, as of `now`.
pub fn read(path: &Path, now: u64) -> Applied {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Applied::defaults("missing")
        }
        Err(_) => return Applied::defaults("unreadable"),
    };
    let Ok(saved) = serde_json::from_slice::<Saved>(&data) else {
        return Applied::defaults("unreadable");
    };
    if saved.format != FORMAT {
        return Applied::defaults("unreadable");
    }
    // A check from the future means the clock was set back: as stale.
    if saved.fetched_at > now || now - saved.fetched_at > MAX_AGE {
        return Applied::defaults("stale");
    }
    Applied {
        rollout: Rollout::parse(&saved.rollout),
        config: parse_config(Some(&saved.config)),
    }
}

/// Saves what a fetched manifest says for the next app run. A manifest that
/// isn't JSON, or has no Windows entry, saves nothing to apply: defaults.
pub fn write(path: &Path, manifest: &[u8], now: u64) -> std::io::Result<()> {
    let windows = serde_json::from_slice::<Value>(manifest)
        .ok()
        .filter(|manifest| manifest.get("schema").and_then(Value::as_u64) == Some(1))
        .and_then(|mut manifest| manifest.get_mut("windows").map(Value::take))
        .unwrap_or_default();
    let saved = Saved {
        format: FORMAT,
        fetched_at: now,
        rollout: windows.get("rollout").cloned().unwrap_or_default(),
        config: windows.get("config").cloned().unwrap_or_default(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&saved)?)?;
    std::fs::rename(tmp, path)
}

fn path() -> Option<PathBuf> {
    crate::settings::app_data_dir().map(|dir| dir.join(FILE_NAME))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

static APPLIED: OnceLock<Applied> = OnceLock::new();
static SS_RULE: OnceLock<bool> = OnceLock::new();

/// Reads the saved config once, at launch, before the second screen's
/// memory loads. Later calls, and [`current`] before it, see the defaults.
pub fn load() {
    let applied = path().map_or_else(|| Applied::defaults("missing"), |p| read(&p, now_secs()));
    let _ = APPLIED.set(applied);
}

/// What this app run applies; the defaults before [`load`].
pub fn current() -> &'static Applied {
    static DEFAULTS: OnceLock<Applied> = OnceLock::new();
    APPLIED
        .get()
        .unwrap_or_else(|| DEFAULTS.get_or_init(|| Applied::defaults("missing")))
}

/// The second screen's memory found whether a rule names this PC.
#[cfg(windows)]
pub(crate) fn note_second_screen_rule(matched: bool) {
    let _ = SS_RULE.set(matched);
}

/// The one log line per app run, after the second screen's memory loaded.
pub fn log_launch() {
    tracing::info!("{}", current().summary(SS_RULE.get().copied()));
}

/// Called by the update worker with each manifest it fetched.
pub fn save_fetched(manifest: &[u8]) {
    if let Some(path) = path() {
        if let Err(error) = write(&path, manifest, now_secs()) {
            tracing::warn!(%error, "Could not save the update manifest's settings");
        }
    }
}

#[cfg(test)]
mod tests;
