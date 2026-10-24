//! Turns VideoToolbox's length-prefixed samples into the Annex B access
//! units the transport and the iPad decoder take, and reads back the
//! profile, tier and level the encoder actually wrote.

use super::config::Codec;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MalformedSample;

impl std::fmt::Display for MalformedSample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("encoder sample has a NAL length past its end")
    }
}

/// Rewrites 4-byte big-endian NAL lengths as 4-byte start codes, in place.
/// VideoToolbox always uses a 4-byte length (`nal_unit_header_length` 4).
pub fn length_prefixed_to_annex_b(data: &mut [u8]) -> Result<(), MalformedSample> {
    let mut at = 0;
    while at < data.len() {
        let header = data.get(at..at + 4).ok_or(MalformedSample)?;
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let end = (at + 4).checked_add(length).ok_or(MalformedSample)?;
        if length == 0 || end > data.len() {
            return Err(MalformedSample);
        }
        data[at..at + 4].copy_from_slice(&START_CODE);
        at = end;
    }
    Ok(())
}

/// One access unit: on a keyframe the parameter sets (HEVC VPS, SPS, PPS or
/// H.264 SPS, PPS) come first, so a viewer can join on any IDR. The iPad
/// detects HEVC from that VPS (`VideoDecoder.sniffCodec`).
pub fn access_unit(
    parameter_sets: &[Vec<u8>],
    mut sample: Vec<u8>,
) -> Result<Vec<u8>, MalformedSample> {
    length_prefixed_to_annex_b(&mut sample)?;
    if parameter_sets.is_empty() {
        return Ok(sample);
    }
    let header_bytes: usize = parameter_sets.iter().map(|set| set.len() + 4).sum();
    let mut unit = Vec::with_capacity(header_bytes + sample.len());
    for set in parameter_sets {
        unit.extend_from_slice(&START_CODE);
        unit.extend_from_slice(set);
    }
    unit.extend_from_slice(&sample);
    Ok(unit)
}

/// Profile, tier and level as written in a parameter set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileTierLevel {
    /// HEVC `general_profile_idc` (1 = Main) or H.264 `profile_idc` (100 = High).
    pub profile_idc: u8,
    /// HEVC `general_tier_flag`; always false for H.264, which has no tiers.
    pub high_tier: bool,
    /// HEVC `general_level_idc` (30 x level) or H.264 `level_idc` (10 x level).
    pub level_idc: u8,
}

impl ProfileTierLevel {
    pub fn describe(&self, codec: Codec) -> String {
        match codec {
            Codec::Hevc => format!(
                "{} {} tier {}.{}",
                match self.profile_idc {
                    1 => "Main".to_string(),
                    2 => "Main 10".to_string(),
                    other => format!("profile {other}"),
                },
                if self.high_tier { "High" } else { "Main" },
                self.level_idc / 30,
                (self.level_idc % 30) / 3
            ),
            Codec::H264 => format!(
                "{} {}.{}",
                match self.profile_idc {
                    66 => "Baseline".to_string(),
                    77 => "Main".to_string(),
                    100 => "High".to_string(),
                    other => format!("profile {other}"),
                },
                self.level_idc / 10,
                self.level_idc % 10
            ),
        }
    }
}

/// Removes emulation-prevention bytes (00 00 03 -> 00 00).
fn rbsp(nal: &[u8]) -> Vec<u8> {
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

/// `profile_tier_level()` starts at `at`: 1 byte of profile space, tier and
/// profile, 4 bytes of compatibility flags, 6 of constraint flags, then
/// `general_level_idc`.
fn hevc_ptl(raw: &[u8], at: usize) -> Option<ProfileTierLevel> {
    let first = *raw.get(at)?;
    Some(ProfileTierLevel {
        profile_idc: first & 0x1f,
        high_tier: first & 0x20 != 0,
        level_idc: *raw.get(at + 11)?,
    })
}

/// The profile, tier and level of one parameter-set NAL (no start code).
/// Reads an HEVC VPS or SPS, or an H.264 SPS; anything else is `None`.
pub fn parameter_set_ptl(codec: Codec, nal: &[u8]) -> Option<ProfileTierLevel> {
    let raw = rbsp(nal);
    match codec {
        Codec::Hevc => match (raw.first()? >> 1) & 0x3f {
            // VPS: 2-byte NAL header, 4 bytes of VPS fields.
            32 => hevc_ptl(&raw, 6),
            // SPS: 2-byte NAL header, 1 byte of SPS fields.
            33 => hevc_ptl(&raw, 3),
            _ => None,
        },
        Codec::H264 => (raw.first()? & 0x1f == 7 && raw.len() >= 4).then(|| ProfileTierLevel {
            profile_idc: raw[1],
            high_tier: false,
            level_idc: raw[3],
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-built VPS: Main profile, High tier, level 5.2 (idc 156), with
    /// the emulation-prevention bytes a real encoder must insert. The
    /// hardware test checks this parser against FFmpeg on real output.
    const HEVC_VPS_5_2_HIGH: [u8; 21] = [
        0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x21, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00,
        0x03, 0x00, 0x00, 0x03, 0x00, 0x9c,
    ];
    /// The matching SPS, Main tier, level 5.1 (idc 153).
    const HEVC_SPS_5_1_MAIN: [u8; 18] = [
        0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x99,
    ];

    #[test]
    fn length_prefixes_become_start_codes() {
        let mut data = vec![0, 0, 0, 2, 0x26, 0x01, 0, 0, 0, 3, 0x02, 0x01, 0xaa];
        length_prefixed_to_annex_b(&mut data).unwrap();
        assert_eq!(data, [0, 0, 0, 1, 0x26, 0x01, 0, 0, 0, 1, 0x02, 0x01, 0xaa]);
    }

    #[test]
    fn truncated_or_empty_nals_are_rejected() {
        assert_eq!(
            length_prefixed_to_annex_b(&mut [0, 0, 0, 5, 1, 2]),
            Err(MalformedSample)
        );
        assert_eq!(
            length_prefixed_to_annex_b(&mut [0, 0, 0, 0]),
            Err(MalformedSample)
        );
        assert_eq!(
            length_prefixed_to_annex_b(&mut [0, 0, 1]),
            Err(MalformedSample)
        );
        assert_eq!(
            length_prefixed_to_annex_b(&mut [0xff, 0xff, 0xff, 0xff, 1]),
            Err(MalformedSample)
        );
        assert_eq!(length_prefixed_to_annex_b(&mut []), Ok(()));
    }

    #[test]
    fn keyframes_carry_parameter_sets_ahead_of_the_slices() {
        let sets = vec![vec![0x40, 0x01], vec![0x42, 0x01], vec![0x44, 0x01]];
        let unit = access_unit(&sets, vec![0, 0, 0, 3, 0x26, 0x01, 0x99]).unwrap();
        assert_eq!(
            unit,
            [
                0, 0, 0, 1, 0x40, 0x01, 0, 0, 0, 1, 0x42, 0x01, 0, 0, 0, 1, 0x44, 0x01, 0, 0, 0, 1,
                0x26, 0x01, 0x99
            ]
        );
        assert!(eternal_wire::hevc::contains_parameter_sets(&unit));
        assert!(eternal_wire::hevc::contains_keyframe(&unit));
        // Delta frames stay as they are.
        let delta = access_unit(&[], vec![0, 0, 0, 2, 0x02, 0x01]).unwrap();
        assert_eq!(delta, [0, 0, 0, 1, 0x02, 0x01]);
        assert!(!eternal_wire::hevc::contains_keyframe(&delta));
    }

    #[test]
    fn reads_hevc_tier_and_level_from_the_vps_and_sps() {
        let ptl = parameter_set_ptl(Codec::Hevc, &HEVC_VPS_5_2_HIGH).unwrap();
        assert_eq!(
            ptl,
            ProfileTierLevel {
                profile_idc: 1,
                high_tier: true,
                level_idc: 156
            }
        );
        assert_eq!(ptl.describe(Codec::Hevc), "Main High tier 5.2");
        let ptl = parameter_set_ptl(Codec::Hevc, &HEVC_SPS_5_1_MAIN).unwrap();
        assert_eq!((ptl.profile_idc, ptl.high_tier), (1, false));
        assert_eq!(ptl.level_idc, 0x99);
        assert_eq!(ptl.describe(Codec::Hevc), "Main Main tier 5.1");
        // Slices and PPS carry no level.
        assert_eq!(parameter_set_ptl(Codec::Hevc, &[0x44, 0x01, 0xc0]), None);
        assert_eq!(parameter_set_ptl(Codec::Hevc, &[]), None);
    }

    #[test]
    fn reads_h264_profile_and_level_from_the_sps() {
        let ptl = parameter_set_ptl(Codec::H264, &[0x27, 100, 0x00, 52, 0xac]).unwrap();
        assert_eq!(ptl.describe(Codec::H264), "High 5.2");
        assert_eq!(parameter_set_ptl(Codec::H264, &[0x28, 0xee]), None);
    }
}
