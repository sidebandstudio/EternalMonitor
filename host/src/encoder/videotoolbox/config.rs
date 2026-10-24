//! Pure session planning for the direct VideoToolbox encoder: which codec,
//! which level a stream needs, the frame rate a level can carry, and the
//! rate-control values. No VideoToolbox calls, so all of it is unit tested.

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Hevc,
    H264,
}

impl Codec {
    /// HEVC whenever the client can decode it. H.264 only for a client that
    /// advertises no HEVC decode (or a user who pinned H.264).
    pub fn negotiate(client_decodes_hevc: bool) -> Self {
        if client_decodes_hevc {
            Self::Hevc
        } else {
            Self::H264
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Hevc => "HEVC (VideoToolbox)",
            Self::H264 => "H.264 (VideoToolbox)",
        }
    }
}

/// A level from H.264 Table A-1 or HEVC Table A.8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    pub name: &'static str,
    /// H.264 `level_idc` (10 x level) or HEVC `general_level_idc` (30 x level).
    pub idc: u8,
}

/// HEVC tier. H.264 has none; its plans say `Main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Main,
    High,
}

/// One row of a level table.
struct LevelLimits {
    level: Level,
    /// H.264 MaxFS (macroblocks) or HEVC MaxLumaPs (luma samples).
    max_picture: u64,
    /// H.264 MaxMBPS or HEVC MaxLumaSr.
    max_rate: u64,
    /// MaxBR and MaxCPB in units of the codec's factor, Main tier then High
    /// tier. 0 where a level has no High tier, and for H.264's second entry.
    max_br: [u64; 2],
    max_cpb: [u64; 2],
}

const fn row(
    name: &'static str,
    idc: u8,
    max_picture: u64,
    max_rate: u64,
    max_br: [u64; 2],
    max_cpb: [u64; 2],
) -> LevelLimits {
    LevelLimits {
        level: Level { name, idc },
        max_picture,
        max_rate,
        max_br,
        max_cpb,
    }
}

/// H.264 Table A-1: MaxFS, MaxMBPS, MaxBR, MaxCPB.
const H264_LEVELS: &[LevelLimits] = &[
    row("3.0", 30, 1_620, 40_500, [10_000, 0], [10_000, 0]),
    row("3.1", 31, 3_600, 108_000, [14_000, 0], [14_000, 0]),
    row("3.2", 32, 5_120, 216_000, [20_000, 0], [20_000, 0]),
    row("4.0", 40, 8_192, 245_760, [20_000, 0], [25_000, 0]),
    row("4.1", 41, 8_192, 245_760, [50_000, 0], [62_500, 0]),
    row("4.2", 42, 8_704, 522_240, [50_000, 0], [62_500, 0]),
    row("5.0", 50, 22_080, 589_824, [135_000, 0], [135_000, 0]),
    row("5.1", 51, 36_864, 983_040, [240_000, 0], [240_000, 0]),
    row("5.2", 52, 36_864, 2_073_600, [240_000, 0], [240_000, 0]),
    row("6.0", 60, 139_264, 4_177_920, [240_000, 0], [240_000, 0]),
    row("6.1", 61, 139_264, 8_355_840, [480_000, 0], [480_000, 0]),
    row("6.2", 62, 139_264, 16_711_680, [800_000, 0], [800_000, 0]),
];

/// HEVC Tables A.8 and A.9: MaxLumaPs, MaxLumaSr, MaxBR and MaxCPB by tier.
const HEVC_LEVELS: &[LevelLimits] = &[
    row("3.0", 90, 552_960, 16_588_800, [6_000, 0], [6_000, 0]),
    row("3.1", 93, 983_040, 33_177_600, [10_000, 0], [10_000, 0]),
    row(
        "4.0",
        120,
        2_228_224,
        66_846_720,
        [12_000, 30_000],
        [12_000, 30_000],
    ),
    row(
        "4.1",
        123,
        2_228_224,
        133_693_440,
        [20_000, 50_000],
        [20_000, 50_000],
    ),
    row(
        "5.0",
        150,
        8_912_896,
        267_386_880,
        [25_000, 100_000],
        [25_000, 100_000],
    ),
    row(
        "5.1",
        153,
        8_912_896,
        534_773_760,
        [40_000, 160_000],
        [40_000, 160_000],
    ),
    row(
        "5.2",
        156,
        8_912_896,
        1_069_547_520,
        [60_000, 240_000],
        [60_000, 240_000],
    ),
    row(
        "6.0",
        180,
        35_651_584,
        1_069_547_520,
        [60_000, 240_000],
        [60_000, 240_000],
    ),
    row(
        "6.1",
        183,
        35_651_584,
        2_139_095_040,
        [120_000, 480_000],
        [120_000, 480_000],
    ),
    row(
        "6.2",
        186,
        35_651_584,
        4_278_190_080,
        [240_000, 800_000],
        [240_000, 800_000],
    ),
];

/// Bits per second per MaxBR unit (and bits per MaxCPB unit): the VCL
/// factors for H.264 High (Table A-2) and HEVC Main (Table A.10). These are
/// what VideoToolbox enforces. It refuses to prepare an H.264 High 3.1
/// session above 17.5 Mbps (14,000 x 1,250), and HEVC AutoLevel steps from
/// 4.0 to 4.1 between 12 and 13 Mbps (12,000 x 1,000), on an M1 Max with
/// macOS 15.7. FFmpeg's `h264_levels.c` uses the larger NAL factor (1,500)
/// in the same check.
const H264_HIGH_BR_FACTOR: u64 = 1_250;
const HEVC_MAIN_BR_FACTOR: u64 = 1_000;

/// The highest H.264 level the host emits. VideoToolbox has no H.264
/// profile constant above 5.2. Above it the host lowers the frame rate.
pub const MAX_H264_LEVEL_IDC: u8 = 52;

/// The fastest H.264 the host plans. On an M1 Max the hardware H.264 encoder
/// managed only 38-62 fps at 2034x1398 and 85 fps at 2670x1878 when asked
/// for 120, although level 5.2 allows the former (`hardware_tests`, spike a).
/// H.264 is only for clients without HEVC decode, so it runs at up to 60.
pub const MAX_H264_FPS: u32 = 60;

/// VideoToolbox has no explicit HEVC levels and no tier setting: AutoLevel
/// picks both when the session is prepared. Its tier follows the luma sample
/// rate alone. Whatever the level or bitrate, it switches to High tier once
/// the 16-aligned rate passes about that of 4K60. Measured on an M1 Max with
/// macOS 15.7 (`hardware_tests::hevc_auto_level_survey`): 2672x1888 (the
/// aligned full Duo size) at 98 fps, 494.4 M/s, stayed Main; at 99 fps
/// (499.4 M/s) it went High, as did 1920x1088 at 240 (501.4 M/s). This is
/// the highest rate measured on Main tier; the encode loop still checks the
/// tier VideoToolbox actually writes.
pub const VT_HEVC_MAIN_TIER_MAX_LUMA_RATE: u64 = 2_672 * 1_888 * 98;

/// The tier VideoToolbox's HEVC AutoLevel writes for this size and rate.
pub fn vt_hevc_tier(width: u32, height: u32, fps: u32) -> Tier {
    let aligned = u64::from(width.next_multiple_of(16)) * u64::from(height.next_multiple_of(16));
    if aligned * u64::from(fps) > VT_HEVC_MAIN_TIER_MAX_LUMA_RATE {
        Tier::High
    } else {
        Tier::Main
    }
}

/// How far HEVC may go. HELLO2 carries no decoder profile, tier or level,
/// and the iOS app advertises HEVC on every device, so a client cannot
/// refuse a stream it cannot decode. The default stays where Apple decoders
/// are safest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HevcLimit {
    /// Main tier, level 5.1 at most: full Duo size runs at up to 60 fps
    /// (90 for a 90 Hz request), the outer screen at 120, and the bitrate
    /// stops at 40 Mbps.
    #[default]
    MainTier,
    /// EXPERIMENTAL: High tier and level 5.2, which full Duo size at 120 fps
    /// needs (VideoToolbox writes Main High tier 5.2 there). No iPad or Duo
    /// decoder has been checked with it. Opt in with
    /// `ETERNAL_VT_EXPERIMENTAL_HEVC_HIGH_TIER=1` until a device has
    /// validated it or the client can advertise its decode level.
    ExperimentalHighTier,
}

pub const EXPERIMENTAL_HEVC_HIGH_TIER_ENV: &str = "ETERNAL_VT_EXPERIMENTAL_HEVC_HIGH_TIER";

impl HevcLimit {
    pub fn from_env() -> Self {
        if std::env::var(EXPERIMENTAL_HEVC_HIGH_TIER_ENV).is_ok_and(|value| value.trim() == "1") {
            Self::ExperimentalHighTier
        } else {
            Self::MainTier
        }
    }

    fn max_level_idc(self) -> u8 {
        match self {
            Self::MainTier => 153,
            Self::ExperimentalHighTier => 156,
        }
    }

    pub fn allows(self, tier: Tier) -> bool {
        tier == Tier::Main || self == Self::ExperimentalHighTier
    }
}

/// What planning may use beyond the stream's own needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub hevc: HevcLimit,
    /// The highest encode rate to plan. Lowered by the encode loop when
    /// VideoToolbox writes a High tier that `hevc` does not allow.
    pub max_fps: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            hevc: HevcLimit::default(),
            max_fps: u32::MAX,
        }
    }
}

fn table(codec: Codec) -> (&'static [LevelLimits], u64) {
    match codec {
        Codec::Hevc => (HEVC_LEVELS, HEVC_MAIN_BR_FACTOR),
        Codec::H264 => (H264_LEVELS, H264_HIGH_BR_FACTOR),
    }
}

/// Whether a level's picture size, dimensions and sample rate cover the
/// stream.
fn covers_picture(codec: Codec, row: &LevelLimits, width: u32, height: u32, fps: u32) -> bool {
    match codec {
        Codec::H264 => {
            let (mb_w, mb_h) = (
                u64::from(width.div_ceil(16)),
                u64::from(height.div_ceil(16)),
            );
            let frame_mbs = mb_w * mb_h;
            // A.3.1 (f): each dimension at most sqrt(8 * MaxFS) macroblocks.
            let max_dim = 8 * row.max_picture;
            frame_mbs <= row.max_picture
                && mb_w * mb_w <= max_dim
                && mb_h * mb_h <= max_dim
                && frame_mbs * u64::from(fps) <= row.max_rate
        }
        Codec::Hevc => {
            let (w, h) = (u64::from(width), u64::from(height));
            let samples = w * h;
            // A.4.1: each dimension at most sqrt(8 * MaxLumaPs).
            samples <= row.max_picture
                && w * w <= 8 * row.max_picture
                && h * h <= 8 * row.max_picture
                && samples * u64::from(fps) <= row.max_rate
        }
    }
}

/// The most a level carries in `tier`, in bits per second (0 for a tier the
/// level lacks).
fn max_bitrate(row: &LevelLimits, tier: Tier, factor: u64) -> u64 {
    row.max_br[tier as usize] * factor
}

/// Whether a level carries `bitrate_bps` in `tier`: the average within
/// MaxBR, and the burst that `DataRateLimits` would allow over its window
/// within what a CPB of MaxCPB, filling at MaxBR, absorbs. At 1.5x the
/// average over one second the CPB never binds before MaxBR does
/// (`tests::the_cpb_never_binds_before_max_br`), but it is checked so a
/// different burst limit cannot break conformance unnoticed.
fn carries(row: &LevelLimits, tier: Tier, factor: u64, bitrate_bps: u32) -> bool {
    let max_br = max_bitrate(row, tier, factor);
    let max_cpb = row.max_cpb[tier as usize] * factor;
    let (burst_bytes, window_seconds) = data_rate_limit(bitrate_bps);
    let drained = (max_br as f64 * window_seconds) as u64;
    max_br > 0 && u64::from(bitrate_bps) <= max_br && burst_bytes as u64 * 8 <= drained + max_cpb
}

/// Lowest level, in `tier`, whose picture size, dimensions, sample rate,
/// bitrate and CPB cover the stream.
pub fn min_level(
    codec: Codec,
    tier: Tier,
    width: u32,
    height: u32,
    fps: u32,
    bitrate_bps: u32,
) -> Option<Level> {
    let (levels, factor) = table(codec);
    levels
        .iter()
        .find(|row| {
            covers_picture(codec, row, width, height, fps)
                && carries(row, tier, factor, bitrate_bps)
        })
        .map(|row| row.level)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// Not even a single frame of this size fits the highest level allowed.
    TooLarge {
        codec: Codec,
        width: u32,
        height: u32,
        max_level: &'static str,
    },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge {
                codec,
                width,
                height,
                max_level,
            } => write!(
                f,
                "{width}x{height} exceeds {} level {max_level} at any frame rate",
                codec.display_name()
            ),
        }
    }
}

impl std::error::Error for PlanError {}

/// What a session is opened with, derived from the codec, size, rate and
/// bitrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionPlan {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    /// The rate the client asked for.
    pub target_fps: u32,
    /// What the encoder runs at: `target_fps`, or a whole divisor of it when
    /// the target needs a level or tier the limits rule out, or is H.264
    /// above [`MAX_H264_FPS`]. Becomes `ExpectedFrameRate`, and is the rate
    /// HELLO_ACK and heartbeats advertise.
    pub encode_fps: u32,
    pub tier: Tier,
    /// The lowest legal level at `encode_fps` and `level_bitrate_bps`.
    pub level: Level,
    /// The bitrate the level is planned for: the ABR ceiling, so ABR steps
    /// below it never outgrow the level, or the most the highest allowed
    /// level carries. The session is prepared at this bitrate, which is when
    /// VideoToolbox fixes the level.
    pub level_bitrate_bps: u32,
    /// The most `level` carries. Live bitrate changes are clamped to it.
    pub max_bitrate_bps: u32,
}

impl SessionPlan {
    /// True when frames arriving at `target_fps` must be thinned.
    pub fn is_rate_capped(&self) -> bool {
        self.encode_fps < self.target_fps
    }

    /// The shortest gap between submitted frames when capped, with a
    /// tenth of an interval of slack for capture jitter.
    pub fn min_frame_interval(&self) -> Option<Duration> {
        self.is_rate_capped()
            .then(|| Duration::from_secs_f64(0.9 / f64::from(self.encode_fps.max(1))))
    }

    /// The bitrate a session of this plan may run at.
    pub fn clamp_bitrate(&self, bitrate_bps: u32) -> u32 {
        bitrate_bps
            .max(MIN_BITRATE_BPS)
            .min(self.max_bitrate_bps.max(MIN_BITRATE_BPS))
    }
}

/// Plans a session for a stream of `width` x `height` at `target_fps`
/// whose bitrate may rise to `ceiling_bps`.
pub fn plan_session(
    codec: Codec,
    width: u32,
    height: u32,
    target_fps: u32,
    ceiling_bps: u32,
    limits: Limits,
) -> Result<SessionPlan, PlanError> {
    let target_fps = target_fps.max(1);
    let ceiling_bps = ceiling_bps.max(MIN_BITRATE_BPS);
    let max_idc = match codec {
        Codec::Hevc => limits.hevc.max_level_idc(),
        Codec::H264 => MAX_H264_LEVEL_IDC,
    };
    let (levels, factor) = table(codec);
    let allowed = || levels.iter().filter(|row| row.level.idc <= max_idc);
    // Divide the rate by the smallest whole factor that fits, so a 120 Hz
    // source thins to every other frame (60) rather than an uneven 105.
    for divisor in 1..=target_fps {
        let fps = target_fps / divisor;
        if divisor > 1 && fps == target_fps / (divisor - 1) {
            continue;
        }
        if fps > limits.max_fps || (codec == Codec::H264 && fps > MAX_H264_FPS) {
            continue;
        }
        let tier = match codec {
            Codec::Hevc => vt_hevc_tier(width, height, fps),
            Codec::H264 => Tier::Main,
        };
        if codec == Codec::Hevc && !limits.hevc.allows(tier) {
            continue;
        }
        // The size and rate decide whether this fps fits; the bitrate only
        // raises the level within what is allowed, and is clamped beyond it.
        let fitting: Vec<&LevelLimits> = allowed()
            .filter(|row| covers_picture(codec, row, width, height, fps))
            .collect();
        let row = fitting
            .iter()
            .find(|row| carries(row, tier, factor, ceiling_bps))
            .or_else(|| {
                fitting
                    .iter()
                    .rfind(|row| max_bitrate(row, tier, factor) > 0)
            });
        let Some(&row) = row else {
            continue;
        };
        let max_bitrate_bps = u32::try_from(max_bitrate(row, tier, factor)).unwrap_or(u32::MAX);
        return Ok(SessionPlan {
            codec,
            width,
            height,
            target_fps,
            encode_fps: fps,
            tier,
            level: row.level,
            level_bitrate_bps: ceiling_bps.min(max_bitrate_bps),
            max_bitrate_bps,
        });
    }
    Err(PlanError::TooLarge {
        codec,
        width,
        height,
        max_level: allowed().next_back().map_or("?", |row| row.level.name),
    })
}

/// What a live session does when the codec, size, rate or bitrate ceiling
/// changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanChange {
    Keep,
    /// Same session; send the new `ExpectedFrameRate`.
    UpdateFrameRate(u32),
    /// VideoToolbox fixes the level when it prepares the session, so a new
    /// codec, a new size, or a rate or bitrate ceiling the opened level or
    /// tier cannot carry needs a new session (which starts on an IDR).
    Reopen,
}

/// `opened` is the plan the session was created with; `live_fps` its current
/// `ExpectedFrameRate`. A lower level or tier than the session's is kept:
/// over-declaring is legal.
pub fn plan_change(opened: &SessionPlan, live_fps: u32, wanted: &SessionPlan) -> PlanChange {
    if opened.codec != wanted.codec
        || opened.width != wanted.width
        || opened.height != wanted.height
        || wanted.level.idc > opened.level.idc
        || wanted.tier > opened.tier
    {
        PlanChange::Reopen
    } else if wanted.encode_fps != live_fps {
        PlanChange::UpdateFrameRate(wanted.encode_fps)
    } else {
        PlanChange::Keep
    }
}

/// The VideoToolbox H.264 High profile constant name for a level. VT has
/// constants from 3.0 to 5.2; smaller streams declare 3.0 (over-declaring
/// is legal, under-declaring makes Apple decoders reject the stream).
pub fn h264_profile_level_name(level: Level) -> Option<&'static str> {
    Some(match level.idc {
        0..=30 => "H264_High_3_0",
        31 => "H264_High_3_1",
        32 => "H264_High_3_2",
        40 => "H264_High_4_0",
        41 => "H264_High_4_1",
        42 => "H264_High_4_2",
        50 => "H264_High_5_0",
        51 => "H264_High_5_1",
        52 => "H264_High_5_2",
        _ => return None,
    })
}

/// Bitrate floor the rest of the host also applies.
pub const MIN_BITRATE_BPS: u32 = 500_000;

/// `DataRateLimits` as (bytes, seconds), when enabled (`Tuning`): at most
/// 1.5x the average over any one-second window. It caps bursts without
/// starving a requested IDR, which is under a tenth of a second's budget at
/// full size.
pub fn data_rate_limit(bitrate_bps: u32) -> (i64, f64) {
    let bps = i64::from(bitrate_bps.max(MIN_BITRATE_BPS));
    (bps * 3 / 2 / 8, 1.0)
}

/// Seconds between IDRs the encoder inserts on its own. Clients ask for a
/// keyframe whenever they lose sync (`KEYFRAME_REQUEST`), so this is only a
/// safety net; the old 30-frame GOP spent 12-13% of the bitrate on IDRs.
pub const KEYFRAME_INTERVAL_SECONDS: u32 = 5;

pub fn max_keyframe_interval_frames(encode_fps: u32) -> i32 {
    i32::try_from(encode_fps.max(1) * KEYFRAME_INTERVAL_SECONDS).unwrap_or(i32::MAX)
}

/// How the encode loop treats a VideoToolbox status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusClass {
    Ok,
    /// The session is gone (sleep, media server reset, GPU change): tear it
    /// down and create a new one.
    SessionLost,
    /// The hardware encoder is busy or briefly unavailable (for example while
    /// the Mac wakes). Retry after a pause.
    RetryLater,
    /// No hardware encoder: never fall back to software.
    NoHardware,
    Failed,
}

pub fn classify_status(status: i32) -> StatusClass {
    use super::ffi::*;
    match status {
        0 => StatusClass::Ok,
        VT_INVALID_SESSION | VT_VIDEO_ENCODER_MALFUNCTION => StatusClass::SessionLost,
        VT_VIDEO_ENCODER_NOT_AVAILABLE_NOW => StatusClass::RetryLater,
        VT_COULD_NOT_FIND_VIDEO_ENCODER | VT_COULD_NOT_CREATE_INSTANCE => StatusClass::NoHardware,
        _ => StatusClass::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: (u32, u32) = (2670, 1878);
    const OUTER: (u32, u32) = (2034, 1398);

    const MBPS: u32 = 1_000_000;

    fn plan(codec: Codec, size: (u32, u32), fps: u32, ceiling: u32) -> SessionPlan {
        plan_session(codec, size.0, size.1, fps, ceiling, Limits::default()).unwrap()
    }

    fn experimental() -> Limits {
        Limits {
            hevc: HevcLimit::ExperimentalHighTier,
            ..Limits::default()
        }
    }

    /// What VideoToolbox's HEVC AutoLevel wrote on an M1 Max, macOS 15.7
    /// (`hardware_tests::hevc_auto_level_survey`), as (width, height, fps,
    /// Mbps, tier, level).
    const VT_HEVC_SURVEY: &[(u32, u32, u32, u32, Tier, &str)] = &[
        (1280, 720, 30, 4, Tier::Main, "3.1"),
        (1280, 720, 30, 12, Tier::Main, "4.0"),
        (1280, 720, 30, 13, Tier::Main, "4.1"),
        (1280, 720, 30, 15, Tier::Main, "4.1"),
        (1280, 720, 30, 24, Tier::Main, "5.0"),
        (1280, 720, 30, 26, Tier::Main, "5.1"),
        (1280, 720, 30, 39, Tier::Main, "5.1"),
        (1280, 720, 30, 41, Tier::Main, "5.2"),
        (1280, 720, 30, 50, Tier::Main, "5.2"),
        (1280, 720, 60, 4, Tier::Main, "4.0"),
        (1280, 720, 240, 4, Tier::Main, "5.0"),
        (1920, 1080, 60, 4, Tier::Main, "4.1"),
        (1920, 1080, 90, 15, Tier::Main, "5.0"),
        (1920, 1080, 240, 4, Tier::High, "5.1"),
        (2048, 1536, 240, 4, Tier::High, "5.2"),
        (2034, 1398, 60, 15, Tier::Main, "5.0"),
        (2034, 1398, 120, 15, Tier::Main, "5.1"),
        (2034, 1398, 120, 50, Tier::Main, "5.2"),
        (2460, 1640, 120, 4, Tier::Main, "5.1"),
        (2500, 1700, 120, 4, Tier::High, "5.1"),
        (2600, 1800, 120, 4, Tier::High, "5.2"),
        (2670, 1878, 30, 15, Tier::Main, "5.0"),
        (2670, 1878, 60, 15, Tier::Main, "5.1"),
        (2670, 1878, 60, 50, Tier::Main, "5.2"),
        (2670, 1878, 98, 4, Tier::Main, "5.1"),
        (2670, 1878, 99, 4, Tier::High, "5.1"),
        (2670, 1878, 100, 50, Tier::High, "5.1"),
        (2670, 1878, 120, 4, Tier::High, "5.2"),
        (2670, 1878, 120, 50, Tier::High, "5.2"),
    ];

    #[test]
    fn hevc_planning_predicts_what_videotoolbox_writes() {
        for &(width, height, fps, mbps, tier, level) in VT_HEVC_SURVEY {
            let predicted_tier = vt_hevc_tier(width, height, fps);
            let predicted = min_level(Codec::Hevc, predicted_tier, width, height, fps, mbps * MBPS)
                .unwrap()
                .name;
            assert_eq!(
                (predicted_tier, predicted),
                (tier, level),
                "{width}x{height}@{fps} {mbps} Mbps"
            );
        }
        assert_eq!(
            min_level(Codec::Hevc, Tier::Main, 7680, 4320, 60, MBPS)
                .unwrap()
                .name,
            "6.1"
        );
        assert_eq!(
            min_level(Codec::Hevc, Tier::Main, 20_000, 16, 30, MBPS),
            None
        );
    }

    #[test]
    fn h264_levels_follow_table_a1_including_bitrate() {
        let level = |size: (u32, u32), fps, bps| {
            min_level(Codec::H264, Tier::Main, size.0, size.1, fps, bps)
                .unwrap()
                .name
        };
        assert_eq!(level(FULL, 60, 15 * MBPS), "5.2");
        assert_eq!(level(FULL, 120, 15 * MBPS), "6.0");
        assert_eq!(level(OUTER, 120, 15 * MBPS), "5.2");
        assert_eq!(level((1920, 1080), 60, 15 * MBPS), "4.2");
        assert_eq!(level((640, 360), 60, 4 * MBPS), "3.1");
        // 4096 px is 256 MBs wide: 256^2 > 8 * 5120 rules out 3.2, although
        // its frame-size budget (5,120 MBs) covers the 4,096-MB frame.
        assert_eq!(level((4096, 256), 30, 4 * MBPS), "4.0");
        // Sol's repro: 1280x720 at 30 fps is 3.1 by size and rate, which
        // carries 14,000 x 1,250 = 17.5 Mbps (VideoToolbox refuses 18 at
        // 3.1). 50 Mbps needs 4.1.
        assert_eq!(level((1280, 720), 30, 15 * MBPS), "3.1");
        assert_eq!(level((1280, 720), 30, 17_500_000), "3.1");
        assert_eq!(level((1280, 720), 30, 18 * MBPS), "3.2");
        assert_eq!(level((1280, 720), 30, 50 * MBPS), "4.1");
        assert_eq!(level((1280, 720), 30, 63 * MBPS), "5.0");
    }

    #[test]
    fn the_cpb_never_binds_before_max_br() {
        for codec in [Codec::H264, Codec::Hevc] {
            let (levels, factor) = table(codec);
            for row in levels {
                for tier in [Tier::Main, Tier::High] {
                    let max = max_bitrate(row, tier, factor);
                    if max == 0 {
                        continue;
                    }
                    let max = u32::try_from(max).unwrap();
                    assert!(
                        carries(row, tier, factor, max),
                        "{codec:?} {}",
                        row.level.name
                    );
                    assert!(!carries(row, tier, factor, max + 1));
                }
            }
        }
        // No High tier below HEVC level 4, and H.264 has none at all.
        assert!(!carries(
            &HEVC_LEVELS[1],
            Tier::High,
            HEVC_MAIN_BR_FACTOR,
            MBPS
        ));
        assert!(!carries(
            &H264_LEVELS[8],
            Tier::High,
            H264_HIGH_BR_FACTOR,
            MBPS
        ));
    }

    #[test]
    fn hevc_defaults_to_main_tier_and_caps_full_size_at_60() {
        let full = plan(Codec::Hevc, FULL, 120, 15 * MBPS);
        assert_eq!(
            (full.encode_fps, full.tier, full.level.name),
            (60, Tier::Main, "5.1")
        );
        assert!(full.is_rate_capped());
        let gap = full.min_frame_interval().unwrap();
        assert!(gap > Duration::from_millis(14) && gap < Duration::from_millis(16));
        // 90 Hz stays under VideoToolbox's High-tier rate.
        let at_90 = plan(Codec::Hevc, FULL, 90, 15 * MBPS);
        assert_eq!((at_90.encode_fps, at_90.tier), (90, Tier::Main));
        // The outer screen keeps 120 on Main tier.
        let outer = plan(Codec::Hevc, OUTER, 120, 15 * MBPS);
        assert_eq!(
            (outer.encode_fps, outer.tier, outer.level.name),
            (120, Tier::Main, "5.1")
        );
        assert!(!outer.is_rate_capped());
        assert_eq!(outer.min_frame_interval(), None);
    }

    #[test]
    fn full_size_hevc_at_120_needs_the_experimental_opt_in() {
        let full =
            plan_session(Codec::Hevc, FULL.0, FULL.1, 120, 15 * MBPS, experimental()).unwrap();
        assert_eq!(
            (full.encode_fps, full.tier, full.level.name),
            (120, Tier::High, "5.2")
        );
        assert!(!full.is_rate_capped());
        // The encode loop lowers max_fps when VideoToolbox writes a tier the
        // plan did not expect.
        let capped = plan_session(
            Codec::Hevc,
            FULL.0,
            FULL.1,
            120,
            15 * MBPS,
            Limits {
                max_fps: 119,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(capped.encode_fps, 60);
    }

    #[test]
    fn the_bitrate_ceiling_raises_the_level_or_is_clamped() {
        // H.264 has room up to 5.2: the level rises with the ceiling.
        let low = plan(Codec::H264, (1280, 720), 30, 15 * MBPS);
        assert_eq!((low.level.name, low.max_bitrate_bps), ("3.1", 17_500_000));
        assert_eq!(low.level_bitrate_bps, 15 * MBPS);
        let high = plan(Codec::H264, (1280, 720), 30, 50 * MBPS);
        assert_eq!(
            (high.level.name, high.level_bitrate_bps),
            ("4.1", 50 * MBPS)
        );
        // Default HEVC stops at Main tier 5.1, which carries 40 Mbps: the
        // ABR ladder's 50 is clamped, not declared at 5.2.
        let hevc = plan(Codec::Hevc, FULL, 60, 50 * MBPS);
        assert_eq!(
            (
                hevc.level.name,
                hevc.level_bitrate_bps,
                hevc.max_bitrate_bps
            ),
            ("5.1", 40 * MBPS, 40 * MBPS)
        );
        assert_eq!(hevc.clamp_bitrate(50 * MBPS), 40 * MBPS);
        assert_eq!(hevc.clamp_bitrate(25 * MBPS), 25 * MBPS);
        assert_eq!(hevc.clamp_bitrate(0), MIN_BITRATE_BPS);
        let small = plan(Codec::Hevc, (1280, 720), 30, 50 * MBPS);
        assert_eq!((small.level.name, small.tier), ("5.1", Tier::Main));
        // With the opt-in, 5.2 Main carries it.
        let opted =
            plan_session(Codec::Hevc, FULL.0, FULL.1, 60, 50 * MBPS, experimental()).unwrap();
        assert_eq!((opted.level.name, opted.tier), ("5.2", Tier::Main));
        assert_eq!(opted.level_bitrate_bps, 50 * MBPS);
    }

    #[test]
    fn h264_full_size_at_120_thins_to_60_at_level_5_2() {
        let full = plan(Codec::H264, FULL, 120, 15 * MBPS);
        assert_eq!(full.encode_fps, 60);
        assert_eq!(full.level.name, "5.2");
        assert!(full.is_rate_capped());
        // The outer screen fits 5.2 at 120, but the hardware does not keep
        // up, so H.264 runs at 60 there too, at the level 60 needs.
        let outer = plan(Codec::H264, OUTER, 120, 15 * MBPS);
        assert_eq!((outer.encode_fps, outer.level.name), (60, "5.1"));
        // 90 Hz has no whole divisor between 60 and 90 except 45.
        assert_eq!(plan(Codec::H264, FULL, 90, 15 * MBPS).encode_fps, 45);
        assert_eq!(
            plan(Codec::H264, (1920, 1080), 60, 15 * MBPS).encode_fps,
            60
        );
    }

    #[test]
    fn a_frame_too_large_for_the_top_level_is_an_error() {
        // 4K is 32,400 MBs, inside 5.2's MaxFS (36,864): H.264 thins 120 to 60.
        let h264_4k = plan(Codec::H264, (3840, 2160), 120, 15 * MBPS);
        assert_eq!((h264_4k.encode_fps, h264_4k.level.name), (60, "5.2"));
        // 4K HEVC at 60 is past VideoToolbox's Main-tier rate: 30 by default.
        assert_eq!(
            plan(Codec::Hevc, (3840, 2160), 60, 15 * MBPS).encode_fps,
            30
        );
        // 8K is 129,600 MBs: no H.264 rate fits 5.2.
        assert!(plan_session(Codec::H264, 7680, 4320, 60, MBPS, Limits::default()).is_err());
        // HEVC 8K needs 6.x at any rate: MaxLumaPs for 5.x is 8.9M < 33.2M.
        let error = plan_session(Codec::Hevc, 7680, 4320, 60, MBPS, Limits::default()).unwrap_err();
        assert_eq!(
            error,
            PlanError::TooLarge {
                codec: Codec::Hevc,
                width: 7680,
                height: 4320,
                max_level: "5.1",
            }
        );
        assert!(error.to_string().contains("7680x4320"));
        assert!(error.to_string().contains("level 5.1"));
    }

    #[test]
    fn planning_never_returns_zero_fps() {
        let tiny = plan(Codec::Hevc, (640, 360), 0, 0);
        assert_eq!(tiny.target_fps, 1);
        assert_eq!(tiny.encode_fps, 1);
        assert_eq!(tiny.level_bitrate_bps, MIN_BITRATE_BPS);
    }

    #[test]
    fn rate_changes_reuse_the_session_unless_the_level_must_rise() {
        let limits = experimental();
        let hevc = |fps| plan_session(Codec::Hevc, FULL.0, FULL.1, fps, 15 * MBPS, limits).unwrap();
        let (at_120, at_60) = (hevc(120), hevc(60));
        // Opened at 120 (High 5.2): dropping to 60 and back is a property change.
        assert_eq!(
            plan_change(&at_120, 120, &at_60),
            PlanChange::UpdateFrameRate(60)
        );
        assert_eq!(
            plan_change(&at_120, 60, &at_120),
            PlanChange::UpdateFrameRate(120)
        );
        assert_eq!(plan_change(&at_120, 120, &at_120), PlanChange::Keep);
        // Opened at 60 (Main 5.1): 120 needs High 5.2, fixed at creation.
        assert_eq!(plan_change(&at_60, 60, &at_120), PlanChange::Reopen);
        // Codec or size changes always reopen.
        let h264 = plan(Codec::H264, FULL, 60, 15 * MBPS);
        assert_eq!(plan_change(&at_60, 60, &h264), PlanChange::Reopen);
        let outer = plan(Codec::Hevc, OUTER, 60, 15 * MBPS);
        assert_eq!(plan_change(&at_120, 120, &outer), PlanChange::Reopen);
    }

    #[test]
    fn a_higher_bitrate_ceiling_reopens_only_when_the_level_must_rise() {
        // Sol's repro: H.264 1280x720 at 30, opened at 15 Mbps (3.1).
        let opened = plan(Codec::H264, (1280, 720), 30, 15 * MBPS);
        let raised = plan(Codec::H264, (1280, 720), 30, 50 * MBPS);
        assert_eq!(plan_change(&opened, 30, &raised), PlanChange::Reopen);
        // Within what 3.1 carries, and back down: same session.
        let within = plan(Codec::H264, (1280, 720), 30, 17 * MBPS);
        assert_eq!(plan_change(&opened, 30, &within), PlanChange::Keep);
        assert_eq!(plan_change(&raised, 30, &opened), PlanChange::Keep);
        // HEVC at full size: 5.1 is the default's top, so a ceiling past
        // 40 Mbps is clamped and keeps the session.
        let hevc = plan(Codec::Hevc, FULL, 60, 15 * MBPS);
        let hevc_raised = plan(Codec::Hevc, FULL, 60, 50 * MBPS);
        assert_eq!(plan_change(&hevc, 60, &hevc_raised), PlanChange::Keep);
        let small = plan(Codec::Hevc, (1280, 720), 30, 15 * MBPS);
        let small_raised = plan(Codec::Hevc, (1280, 720), 30, 30 * MBPS);
        assert_eq!((small.level.name, small_raised.level.name), ("4.1", "5.1"));
        assert_eq!(plan_change(&small, 30, &small_raised), PlanChange::Reopen);
    }

    #[test]
    fn vt_tier_follows_the_aligned_luma_rate() {
        assert_eq!(vt_hevc_tier(FULL.0, FULL.1, 98), Tier::Main);
        assert_eq!(vt_hevc_tier(FULL.0, FULL.1, 99), Tier::High);
        assert_eq!(vt_hevc_tier(FULL.0, FULL.1, 60), Tier::Main);
        assert_eq!(vt_hevc_tier(OUTER.0, OUTER.1, 120), Tier::Main);
        assert_eq!(vt_hevc_tier(1920, 1080, 240), Tier::High);
    }

    #[test]
    fn h264_profile_names_cover_every_plannable_level() {
        for (width, height, fps) in [
            (640, 360, 60),
            (1280, 720, 60),
            (1920, 1080, 60),
            (2560, 1440, 60),
            (OUTER.0, OUTER.1, 120),
            (FULL.0, FULL.1, 120),
            (3840, 2160, 60),
        ] {
            let planned = plan(Codec::H264, (width, height), fps, 50 * MBPS);
            assert!(
                h264_profile_level_name(planned.level).is_some(),
                "{width}x{height}@{fps} planned {}",
                planned.level.name
            );
        }
        assert_eq!(
            h264_profile_level_name(Level {
                name: "6.0",
                idc: 60
            }),
            None
        );
        assert_eq!(
            h264_profile_level_name(Level {
                name: "1.3",
                idc: 13
            }),
            Some("H264_High_3_0")
        );
    }

    #[test]
    fn rate_limit_allows_one_and_a_half_times_the_average() {
        assert_eq!(data_rate_limit(15_000_000), (2_812_500, 1.0));
        assert_eq!(data_rate_limit(0), (93_750, 1.0));
        assert_eq!(max_keyframe_interval_frames(120), 600);
        assert_eq!(max_keyframe_interval_frames(0), 5);
    }

    #[test]
    fn statuses_map_to_recovery_actions() {
        assert_eq!(classify_status(0), StatusClass::Ok);
        assert_eq!(classify_status(-12903), StatusClass::SessionLost);
        assert_eq!(classify_status(-12912), StatusClass::SessionLost);
        assert_eq!(classify_status(-12915), StatusClass::RetryLater);
        assert_eq!(classify_status(-12908), StatusClass::NoHardware);
        assert_eq!(classify_status(-12907), StatusClass::NoHardware);
        assert_eq!(classify_status(-12902), StatusClass::Failed);
    }

    #[test]
    fn hevc_is_preferred_and_h264_only_without_client_support() {
        assert_eq!(Codec::negotiate(true), Codec::Hevc);
        assert_eq!(Codec::negotiate(false), Codec::H264);
    }
}
