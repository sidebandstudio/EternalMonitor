//! Just enough H.264/HEVC parsing to report the profile, tier and level an
//! encoder put in its parameter sets, and whether that level is legal.

/// NAL units of an Annex B stream, start codes removed.
pub fn annex_b_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(data.len(), |&next| next - 3);
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        nals.push(&data[start..end]);
    }
    nals
}

/// Removes emulation-prevention bytes (00 00 03 -> 00 00).
pub fn rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &byte in nal {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileLevel {
    pub nal: &'static str,
    pub profile: String,
    pub level: String,
    /// H.264 level_idc, or HEVC general_level_idc (30 x level).
    pub level_idc: u8,
}

pub fn h264_sps(nal: &[u8]) -> Option<ProfileLevel> {
    let raw = rbsp(nal);
    if raw.first()? & 0x1f != 7 || raw.len() < 4 {
        return None;
    }
    let (profile_idc, constraints, level_idc) = (raw[1], raw[2], raw[3]);
    let profile = match (profile_idc, constraints & 0x40 != 0) {
        (66, true) => "Constrained Baseline".to_string(),
        (66, false) => "Baseline".to_string(),
        (77, _) => "Main".to_string(),
        (100, _) => "High".to_string(),
        (other, _) => format!("profile_idc {other}"),
    };
    // level_idc 11 with constraint_set3 is level 1b; not relevant here.
    Some(ProfileLevel {
        nal: "SPS",
        profile: format!("{profile} (constraints {constraints:#04x})"),
        level: format!("{}.{}", level_idc / 10, level_idc % 10),
        level_idc,
    })
}

fn hevc_ptl(raw: &[u8], at: usize, nal: &'static str) -> Option<ProfileLevel> {
    let first = *raw.get(at)?;
    let tier = if first & 0x20 != 0 { "High" } else { "Main" };
    let profile_idc = first & 0x1f;
    let level_idc = *raw.get(at + 1 + 4 + 6)?;
    let profile = match profile_idc {
        1 => "Main".to_string(),
        2 => "Main 10".to_string(),
        other => format!("profile_idc {other}"),
    };
    Some(ProfileLevel {
        nal,
        profile: format!("{profile}, {tier} tier"),
        level: format!("{}.{}", level_idc / 30, (level_idc % 30) / 3),
        level_idc,
    })
}

pub fn hevc_parameter_set(nal: &[u8]) -> Option<ProfileLevel> {
    let raw = rbsp(nal);
    match (raw.first()? >> 1) & 0x3f {
        // VPS: 2-byte header, then 4 bytes before profile_tier_level.
        32 => hevc_ptl(&raw, 6, "VPS"),
        // SPS: 2-byte header, then 1 byte before profile_tier_level.
        33 => hevc_ptl(&raw, 3, "SPS"),
        _ => None,
    }
}

/// Every VPS/SPS profile and level in an Annex B access unit.
pub fn describe_annex_b(data: &[u8], hevc: bool) -> Vec<ProfileLevel> {
    annex_b_nals(data)
        .into_iter()
        .filter_map(|nal| {
            if hevc {
                hevc_parameter_set(nal)
            } else {
                h264_sps(nal)
            }
        })
        .collect()
}

/// Same, for raw parameter sets from a CMFormatDescription.
pub fn describe_parameter_sets(sets: &[Vec<u8>], hevc: bool) -> Vec<ProfileLevel> {
    sets.iter()
        .filter_map(|nal| {
            if hevc {
                hevc_parameter_set(nal)
            } else {
                h264_sps(nal)
            }
        })
        .collect()
}

/// Lowest H.264 level (Table A-1) whose MaxFS and MaxMBPS cover the stream.
pub fn h264_min_level(width: u32, height: u32, fps: u32) -> (&'static str, u8, u64) {
    let frame_mbs = u64::from(width.div_ceil(16)) * u64::from(height.div_ceil(16));
    let mbps = frame_mbs * u64::from(fps);
    for (name, idc, max_fs, max_mbps) in [
        ("4.0", 40, 8_192, 245_760),
        ("4.1", 41, 8_192, 245_760),
        ("4.2", 42, 8_704, 522_240),
        ("5.0", 50, 22_080, 589_824),
        ("5.1", 51, 36_864, 983_040),
        ("5.2", 52, 36_864, 2_073_600),
        ("6.0", 60, 139_264, 4_177_920),
        ("6.1", 61, 139_264, 8_355_840),
        ("6.2", 62, 139_264, 16_711_680),
    ] {
        if frame_mbs <= max_fs && mbps <= max_mbps {
            return (name, idc, mbps);
        }
    }
    ("none", 0, mbps)
}

/// Lowest HEVC level (Table A.8) whose MaxLumaPs and MaxLumaSr cover the
/// stream, as (name, general_level_idc, luma samples per second).
pub fn hevc_min_level(width: u32, height: u32, fps: u32) -> (&'static str, u8, u64) {
    let samples = u64::from(width) * u64::from(height);
    let rate = samples * u64::from(fps);
    for (name, idc, max_ps, max_sr) in [
        ("4.0", 120, 2_228_224, 66_846_720),
        ("4.1", 123, 2_228_224, 133_693_440),
        ("5.0", 150, 8_912_896, 267_386_880),
        ("5.1", 153, 8_912_896, 534_773_760),
        ("5.2", 156, 8_912_896, 1_069_547_520),
        ("6.0", 180, 35_651_584, 1_069_547_520),
        ("6.1", 183, 35_651_584, 2_139_095_040),
        ("6.2", 186, 35_651_584, 4_278_190_080),
    ] {
        if samples <= max_ps && rate <= max_sr {
            return (name, idc, rate);
        }
    }
    ("none", 0, rate)
}

/// AVCC (4-byte length prefixed) to Annex B, for VideoToolbox's output.
pub fn avcc_to_annex_b(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    let mut i = 0;
    while i + 4 <= data.len() {
        let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        i += 4;
        let end = (i + len).min(data.len());
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[i..end]);
        i = end;
    }
    out
}
