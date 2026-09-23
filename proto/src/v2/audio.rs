//! One 20 ms Opus packet, with its own sequence and the video's stream epoch.

use super::{CommonPrefix, PacketType, WireError, MAX_DGRAM_SIZE};

pub const AUDIO_HEADER_SIZE: usize = 28;
pub const AUDIO_FLAG_DISCONTINUITY: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioHeader {
    pub session_id: u32,
    pub stream_epoch: u32,
    pub audio_seq: u32,
    pub capture_ts_us: u64,
    pub discontinuity: bool,
}

impl AudioHeader {
    pub fn encode(self, opus: &[u8]) -> Result<Vec<u8>, WireError> {
        self.validate(opus.len())?;
        let mut bytes = vec![0; AUDIO_HEADER_SIZE + opus.len()];
        CommonPrefix {
            packet_type: PacketType::Audio,
            flags: if self.discontinuity {
                AUDIO_FLAG_DISCONTINUITY
            } else {
                0
            },
            payload_len: opus.len() as u16,
        }
        .encode_into(&mut bytes);
        bytes[8..12].copy_from_slice(&self.session_id.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.stream_epoch.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.audio_seq.to_le_bytes());
        bytes[20..28].copy_from_slice(&self.capture_ts_us.to_le_bytes());
        bytes[AUDIO_HEADER_SIZE..].copy_from_slice(opus);
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<(Self, &[u8]), WireError> {
        let prefix = CommonPrefix::decode(bytes)?;
        if prefix.packet_type != PacketType::Audio {
            return Err(WireError::InvalidField("packet_type"));
        }
        if bytes.len() < AUDIO_HEADER_SIZE {
            return Err(WireError::Truncated);
        }
        let payload = &bytes[AUDIO_HEADER_SIZE..];
        if usize::from(prefix.payload_len) != payload.len() {
            return Err(WireError::LengthMismatch);
        }
        let header = Self {
            session_id: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            stream_epoch: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            audio_seq: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
            capture_ts_us: u64::from_le_bytes(bytes[20..28].try_into().unwrap()),
            discontinuity: prefix.flags & AUDIO_FLAG_DISCONTINUITY != 0,
        };
        header.validate(payload.len())?;
        Ok((header, payload))
    }

    fn validate(self, payload_len: usize) -> Result<(), WireError> {
        if self.session_id == 0 {
            return Err(WireError::InvalidField("session_id"));
        }
        if self.stream_epoch == 0 {
            return Err(WireError::InvalidField("stream_epoch"));
        }
        if payload_len == 0 || payload_len > MAX_DGRAM_SIZE - AUDIO_HEADER_SIZE {
            return Err(WireError::InvalidField("payload_len"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::{classify, Classified};

    fn header() -> AudioHeader {
        AudioHeader {
            session_id: 0x1234_5678,
            stream_epoch: 7,
            audio_seq: 91,
            capture_ts_us: 0x1234_5678_9ABC_DEF0,
            discontinuity: true,
        }
    }

    #[test]
    fn audio_header_round_trips_and_classifies_separately() {
        let opus = [0xF8, 0xFF, 0xFE];
        let bytes = header().encode(&opus).unwrap();
        assert_eq!(bytes.len(), AUDIO_HEADER_SIZE + opus.len());
        assert_eq!(classify(&bytes), Classified::Audio { flags: 1 });
        assert_eq!(
            AudioHeader::decode(&bytes).unwrap(),
            (header(), opus.as_slice())
        );
        assert!(AudioHeader::decode(
            &AudioHeader {
                discontinuity: false,
                ..header()
            }
            .encode(&opus)
            .unwrap()
        )
        .is_ok_and(|(h, _)| !h.discontinuity));
    }

    #[test]
    fn audio_rejects_invalid_identity_payload_and_lengths() {
        let good = header().encode(&[0xF8, 0xFF, 0xFE]).unwrap();
        for length in 0..good.len() {
            assert!(AudioHeader::decode(&good[..length]).is_err());
        }
        for (offset, field) in [(8, "session_id"), (12, "stream_epoch")] {
            let mut bad = good.clone();
            bad[offset..offset + 4].fill(0);
            assert_eq!(
                AudioHeader::decode(&bad),
                Err(WireError::InvalidField(field))
            );
        }
        let mut wrong_type = good.clone();
        wrong_type[3] = PacketType::Media as u8;
        assert!(AudioHeader::decode(&wrong_type).is_err());
        let mut empty = good[..AUDIO_HEADER_SIZE].to_vec();
        empty[6..8].fill(0);
        assert_eq!(
            AudioHeader::decode(&empty),
            Err(WireError::InvalidField("payload_len"))
        );
        assert!(header().encode(&[]).is_err());
        assert!(header()
            .encode(&vec![0; MAX_DGRAM_SIZE - AUDIO_HEADER_SIZE + 1])
            .is_err());
        let maximum = header()
            .encode(&vec![1; MAX_DGRAM_SIZE - AUDIO_HEADER_SIZE])
            .unwrap();
        assert_eq!(maximum.len(), MAX_DGRAM_SIZE);
        assert!(AudioHeader::decode(&maximum).is_ok());
        let mut oversize = maximum;
        oversize.push(1);
        oversize[6..8]
            .copy_from_slice(&((MAX_DGRAM_SIZE - AUDIO_HEADER_SIZE + 1) as u16).to_le_bytes());
        assert!(AudioHeader::decode(&oversize).is_err());
    }
}
