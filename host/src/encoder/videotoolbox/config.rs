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

/// H.264 Table A-1: (level, MaxFS in macroblocks, MaxMBPS).
const H264_LEVELS: &[(Level, u64, u64)] = &[
    (lvl("3.0", 30), 1_620, 40_500),
    (lvl("3.1", 31), 3_600, 108_000),
    (lvl("3.2", 32), 5_120, 216_000),
    (lvl("4.0", 40), 8_192, 245_760),
    (lvl("4.1", 41), 8_192, 245_760),
    (lvl("4.2", 42), 8_704, 522_240),
    (lvl("5.0", 50), 22_080, 589_824),
    (lvl("5.1", 51), 36_864, 983_040),
    (lvl("5.2", 52), 36_864, 2_073_600),
    (lvl("6.0", 60), 139_264, 4_177_920),
    (lvl("6.1", 61), 139_264, 8_355_840),
    (lvl("6.2", 62), 139_264, 16_711_680),
];

/// HEVC Table A.8: (level, MaxLumaPs, MaxLumaSr).
const HEVC_LEVELS: &[(Level, u64, u64)] = &[
    (lvl("3.0", 90), 552_960, 16_588_800),
    (lvl("3.1", 93), 983_040, 33_177_600),
    (lvl("4.0", 120), 2_228_224, 66_846_720),
    (lvl("4.1", 123), 2_228_224, 133_693_440),
    (lvl("5.0", 150), 8_912_896, 267_386_880),
    (lvl("5.1", 153), 8_912_896, 534_773_760),
    (lvl("5.2", 156), 8_912_896, 1_069_547_520),
    (lvl("6.0", 180), 35_651_584, 1_069_547_520),
    (lvl("6.1", 183), 35_651_584, 2_139_095_040),
    (lvl("6.2", 186), 35_651_584, 4_278_190_080),
];

const fn lvl(name: &'static str, idc: u8) -> Level {
    Level { name, idc }
}

/// The highest level the host will emit: 5.2 for both codecs. VideoToolbox
/// has no H.264 profile constant above 5.2, and 5.2 is the top level Apple's
/// mobile hardware decoders are known to take (iPhone records HEVC 4K120,
/// which is 5.2). Above it the host lowers the frame rate instead.
pub const MAX_H264_LEVEL_IDC: u8 = 52;
pub const MAX_HEVC_LEVEL_IDC: u8 = 156;

/// The fastest H.264 the host plans. On an M1 Max the hardware H.264 encoder
/// managed only 38-62 fps at 2034x1398 and 85 fps at 2670x1878 when asked
/// for 120, although level 5.2 allows the former (`hardware_tests`, spike a).
/// H.264 is only for clients without HEVC decode, so it runs at up to 60.
pub const MAX_H264_FPS: u32 = 60;

/// Lowest H.264 level whose frame size, frame dimensions and macroblock
/// rate cover the stream.
pub fn h264_min_level(width: u32, height: u32, fps: u32) -> Option<Level> {
    let (mb_w, mb_h) = (
        u64::from(width.div_ceil(16)),
        u64::from(height.div_ceil(16)),
    );
    let frame_mbs = mb_w * mb_h;
    let mbps = frame_mbs * u64::from(fps);
    H264_LEVELS
        .iter()
        .find(|&&(_, max_fs, max_mbps)| {
            // A.3.1 (f): each dimension at most sqrt(8 * MaxFS) macroblocks.
            let max_dim = 8 * max_fs;
            frame_mbs <= max_fs
                && mb_w * mb_w <= max_dim
                && mb_h * mb_h <= max_dim
                && mbps <= max_mbps
        })
        .map(|&(level, ..)| level)
}

/// Lowest HEVC level whose picture size, dimensions and luma sample rate
/// cover the stream.
pub fn hevc_min_level(width: u32, height: u32, fps: u32) -> Option<Level> {
    let (w, h) = (u64::from(width), u64::from(height));
    let samples = w * h;
    let rate = samples * u64::from(fps);
    HEVC_LEVELS
        .iter()
        .find(|&&(_, max_ps, max_sr)| {
            // A.4.1: each dimension at most sqrt(8 * MaxLumaPs).
            samples <= max_ps && w * w <= 8 * max_ps && h * h <= 8 * max_ps && rate <= max_sr
        })
        .map(|&(level, ..)| level)
}

fn min_level(codec: Codec, width: u32, height: u32, fps: u32) -> Option<Level> {
    match codec {
        Codec::Hevc => hevc_min_level(width, height, fps),
        Codec::H264 => h264_min_level(width, height, fps),
    }
}

fn max_level_idc(codec: Codec) -> u8 {
    match codec {
        Codec::Hevc => MAX_HEVC_LEVEL_IDC,
        Codec::H264 => MAX_H264_LEVEL_IDC,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// Not even a single frame of this size fits the codec's top level.
    TooLarge {
        codec: Codec,
        width: u32,
        height: u32,
    },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge {
                codec,
                width,
                height,
            } => write!(
                f,
                "{width}x{height} exceeds {} level 5.2 at any frame rate",
                codec.display_name()
            ),
        }
    }
}

impl std::error::Error for PlanError {}

/// What a session is opened with, derived from the codec, size and rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionPlan {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    /// The rate the client asked for.
    pub target_fps: u32,
    /// What the encoder runs at: `target_fps`, or a whole divisor of it when
    /// the target needs a level above 5.2 or is H.264 above
    /// [`MAX_H264_FPS`]. Becomes `ExpectedFrameRate`.
    pub encode_fps: u32,
    /// The lowest legal level at `encode_fps`.
    pub level: Level,
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
}

pub fn plan_session(
    codec: Codec,
    width: u32,
    height: u32,
    target_fps: u32,
) -> Result<SessionPlan, PlanError> {
    let target_fps = target_fps.max(1);
    let too_large = PlanError::TooLarge {
        codec,
        width,
        height,
    };
    let max_idc = max_level_idc(codec);
    // Divide the rate by the smallest whole factor that fits, so a 120 Hz
    // source thins to every other frame (60) rather than an uneven 105.
    for divisor in 1..=target_fps {
        let fps = target_fps / divisor;
        if divisor > 1 && fps == target_fps / (divisor - 1) {
            continue;
        }
        if codec == Codec::H264 && fps > MAX_H264_FPS {
            continue;
        }
        match min_level(codec, width, height, fps) {
            Some(level) if level.idc <= max_idc => {
                return Ok(SessionPlan {
                    codec,
                    width,
                    height,
                    target_fps,
                    encode_fps: fps,
                    level,
                });
            }
            _ => {}
        }
    }
    Err(too_large)
}

/// What a live session does when the codec, size or rate changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanChange {
    Keep,
    /// Same session; send the new `ExpectedFrameRate`.
    UpdateFrameRate(u32),
    /// VideoToolbox fixes the level when it creates the session, so a new
    /// codec, a new size, or a rate the opened level cannot carry needs a
    /// new session (which starts on an IDR).
    Reopen,
}

/// `opened` is the plan the session was created with; `live_fps` its current
/// `ExpectedFrameRate`.
pub fn plan_change(opened: &SessionPlan, live_fps: u32, wanted: &SessionPlan) -> PlanChange {
    if opened.codec != wanted.codec
        || opened.width != wanted.width
        || opened.height != wanted.height
        || wanted.level.idc > opened.level.idc
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

    #[test]
    fn hevc_levels_match_the_spike_measurements() {
        // RESULTS.md: VT's auto level emitted these with ExpectedFrameRate set.
        assert_eq!(hevc_min_level(FULL.0, FULL.1, 60).unwrap().name, "5.1");
        assert_eq!(hevc_min_level(FULL.0, FULL.1, 120).unwrap().name, "5.2");
        assert_eq!(hevc_min_level(OUTER.0, OUTER.1, 120).unwrap().name, "5.1");
        assert_eq!(hevc_min_level(1920, 1080, 60).unwrap().name, "4.1");
        assert_eq!(hevc_min_level(3840, 2160, 120).unwrap().idc, 156);
        assert_eq!(hevc_min_level(7680, 4320, 60).unwrap().name, "6.1");
        assert_eq!(hevc_min_level(20_000, 16, 30), None);
    }

    #[test]
    fn h264_levels_match_table_a1() {
        assert_eq!(h264_min_level(FULL.0, FULL.1, 60).unwrap().name, "5.2");
        assert_eq!(h264_min_level(FULL.0, FULL.1, 120).unwrap().name, "6.0");
        assert_eq!(h264_min_level(OUTER.0, OUTER.1, 120).unwrap().name, "5.2");
        assert_eq!(h264_min_level(1920, 1080, 60).unwrap().name, "4.2");
        assert_eq!(h264_min_level(1280, 720, 30).unwrap().name, "3.1");
        assert_eq!(h264_min_level(640, 360, 60).unwrap().name, "3.1");
        // 4096 px is 256 MBs wide: 256^2 > 8 * 5120 rules out 3.2, although
        // its frame-size budget (5,120 MBs) covers the 4,096-MB frame.
        assert_eq!(h264_min_level(4096, 256, 30).unwrap().name, "4.0");
    }

    #[test]
    fn hevc_full_size_at_120_needs_no_cap() {
        let plan = plan_session(Codec::Hevc, FULL.0, FULL.1, 120).unwrap();
        assert_eq!(plan.encode_fps, 120);
        assert_eq!(plan.level.idc, 156);
        assert!(!plan.is_rate_capped());
        assert_eq!(plan.min_frame_interval(), None);
    }

    #[test]
    fn h264_full_size_at_120_thins_to_60_at_level_5_2() {
        let plan = plan_session(Codec::H264, FULL.0, FULL.1, 120).unwrap();
        assert_eq!(plan.encode_fps, 60);
        assert_eq!(plan.level.name, "5.2");
        assert!(plan.is_rate_capped());
        let gap = plan.min_frame_interval().unwrap();
        assert!(gap > Duration::from_millis(14) && gap < Duration::from_millis(16));
        // The outer screen fits 5.2 at 120, but the hardware does not keep
        // up, so H.264 runs at 60 there too, at the level 60 needs.
        let outer = plan_session(Codec::H264, OUTER.0, OUTER.1, 120).unwrap();
        assert_eq!((outer.encode_fps, outer.level.name), (60, "5.1"));
        // 90 Hz has no whole divisor between 60 and 90 except 45.
        assert_eq!(
            plan_session(Codec::H264, FULL.0, FULL.1, 90)
                .unwrap()
                .encode_fps,
            45
        );
        assert_eq!(
            plan_session(Codec::H264, 1920, 1080, 60)
                .unwrap()
                .encode_fps,
            60
        );
    }

    #[test]
    fn a_frame_too_large_for_level_5_2_is_an_error() {
        // 4K is 32,400 MBs, inside 5.2's MaxFS (36,864): H.264 thins 120 to 60.
        let h264_4k = plan_session(Codec::H264, 3840, 2160, 120).unwrap();
        assert_eq!((h264_4k.encode_fps, h264_4k.level.name), (60, "5.2"));
        // 8K is 129,600 MBs: no H.264 rate fits 5.2.
        assert!(plan_session(Codec::H264, 7680, 4320, 60).is_err());
        // HEVC 8K needs 6.x at any rate: MaxLumaPs for 5.2 is 8.9M < 33.2M.
        let error = plan_session(Codec::Hevc, 7680, 4320, 60).unwrap_err();
        assert_eq!(
            error,
            PlanError::TooLarge {
                codec: Codec::Hevc,
                width: 7680,
                height: 4320
            }
        );
        assert!(error.to_string().contains("7680x4320"));
    }

    #[test]
    fn planning_never_returns_zero_fps() {
        let plan = plan_session(Codec::Hevc, 640, 360, 0).unwrap();
        assert_eq!(plan.target_fps, 1);
        assert_eq!(plan.encode_fps, 1);
    }

    #[test]
    fn rate_changes_reuse_the_session_unless_the_level_must_rise() {
        let at_120 = plan_session(Codec::Hevc, FULL.0, FULL.1, 120).unwrap();
        let at_60 = plan_session(Codec::Hevc, FULL.0, FULL.1, 60).unwrap();
        // Opened at 120 (5.2): dropping to 60 and back is a property change.
        assert_eq!(
            plan_change(&at_120, 120, &at_60),
            PlanChange::UpdateFrameRate(60)
        );
        assert_eq!(
            plan_change(&at_120, 60, &at_120),
            PlanChange::UpdateFrameRate(120)
        );
        assert_eq!(plan_change(&at_120, 120, &at_120), PlanChange::Keep);
        // Opened at 60 (5.1): 120 needs 5.2, which VT fixed at creation.
        assert_eq!(plan_change(&at_60, 60, &at_120), PlanChange::Reopen);
        // Codec or size changes always reopen.
        let h264 = plan_session(Codec::H264, FULL.0, FULL.1, 60).unwrap();
        assert_eq!(plan_change(&at_60, 60, &h264), PlanChange::Reopen);
        let outer = plan_session(Codec::Hevc, OUTER.0, OUTER.1, 60).unwrap();
        assert_eq!(plan_change(&at_120, 120, &outer), PlanChange::Reopen);
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
            let plan = plan_session(Codec::H264, width, height, fps).unwrap();
            assert!(
                h264_profile_level_name(plan.level).is_some(),
                "{width}x{height}@{fps} planned {}",
                plan.level.name
            );
        }
        assert_eq!(h264_profile_level_name(lvl("6.0", 60)), None);
        assert_eq!(
            h264_profile_level_name(lvl("1.3", 13)),
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
