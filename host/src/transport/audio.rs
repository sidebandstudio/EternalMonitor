use eternal_wire::v2::audio::AudioHeader;

use crate::audio::SessionPacket;
use crate::control::SharedControl;
use crate::stats::PIPELINE_STATS;

use super::link::PeerId;
use super::usb::TransportLinks;

#[derive(Default)]
pub(super) struct AudioSender {
    last_stream: Option<(u32, u32)>,
    reset_pending: bool,
}

impl AudioSender {
    pub fn prepare(
        &mut self,
        queued: SessionPacket,
        shared: &SharedControl,
        epoch: u32,
        now_us: u64,
    ) -> Option<(PeerId, Vec<u8>)> {
        if !shared
            .audio_enabled
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return None;
        }
        let peer = {
            let session = shared.session.lock();
            if session.audio_session_id() != Some(queued.session_id) {
                return None;
            }
            session.peer()?
        };
        if now_us.saturating_sub(queued.packet.capture_ts_us) > 100_000 {
            self.reset_pending = true;
            PIPELINE_STATS.lock().audio.dropped += 1;
            return None;
        }
        let stream = (queued.session_id, epoch);
        let header = AudioHeader {
            session_id: queued.session_id,
            stream_epoch: epoch,
            audio_seq: queued.packet.seq,
            capture_ts_us: queued.packet.capture_ts_us,
            discontinuity: queued.packet.discontinuity
                || self.reset_pending
                || self.last_stream != Some(stream),
        };
        let datagram = header.encode(&queued.packet.opus).ok()?;
        if datagram.len() > shared.max_dgram.load(std::sync::atomic::Ordering::SeqCst) as usize {
            self.reset_pending = true;
            PIPELINE_STATS.lock().audio.dropped += 1;
            return None;
        }
        Some((peer, datagram))
    }

    pub async fn send(
        &mut self,
        queued: SessionPacket,
        links: &TransportLinks,
        shared: &SharedControl,
        epoch: u32,
    ) {
        let session_id = queued.session_id;
        let payload_size = queued.packet.opus.len();
        if let Some((peer, datagram)) =
            self.prepare(queued, shared, epoch, crate::clock::host_now_us())
        {
            if links.send_to(&datagram, peer).await.is_ok() {
                self.last_stream = Some((session_id, epoch));
                self.reset_pending = false;
                PIPELINE_STATS.lock().audio.record_sent(payload_size);
            } else {
                self.reset_pending = true;
                PIPELINE_STATS.lock().audio.dropped += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eternal_wire::v2::control::*;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    fn session(caps: u16) -> (SharedControl, u32) {
        let shared = SharedControl::new(9876, 15_000_000);
        shared.pairing.lock().required = false;
        let config = super::super::SharedConfigSource {
            shared: &shared,
            stream_epoch: 7,
        };
        let peer = PeerId::udp("127.0.0.1:19891".parse().unwrap());
        shared.session.lock().handle_control(
            peer,
            0,
            ControlMessage::Hello2(Hello2 {
                proto_min: 2,
                proto_max: 2,
                client_nonce: 1,
                listen_port: 19891,
                decoder_caps: CAP_DECODE_H264,
                feature_caps: caps,
                screen_px_w: 640,
                screen_px_h: 360,
                screen_pt_w: 640,
                screen_pt_h: 360,
                refresh_hz: 60,
                device_name: "audio test".into(),
                device_id: 11,
                preferred_fps: 60,
                auth_token: [0; 16],
                pairing_code: 0,
            }),
            &config,
            Instant::now(),
        );
        let id = shared.session.lock().session_id().unwrap();
        (shared, id)
    }

    fn packet(id: u32, time: u64) -> SessionPacket {
        SessionPacket {
            session_id: id,
            packet: crate::audio::AudioPacket {
                seq: 3,
                capture_ts_us: time,
                discontinuity: false,
                opus: vec![0xF8, 0xFF, 0xFE],
            },
        }
    }

    #[test]
    fn audio_requires_both_peers_and_the_current_session_and_epoch() {
        let mut sender = AudioSender::default();
        let (legacy, id) = session(0);
        assert!(crate::audio::wanted_session(&legacy).is_none());
        assert!(sender.prepare(packet(id, 100), &legacy, 7, 100).is_none());
        let (shared, id) = session(FEATURE_WANTS_AUDIO);
        assert_eq!(crate::audio::wanted_session(&shared), Some(id));
        shared.audio_enabled.store(false, Ordering::SeqCst);
        assert!(sender.prepare(packet(id, 100), &shared, 7, 100).is_none());
        shared.audio_enabled.store(true, Ordering::SeqCst);
        assert!(sender
            .prepare(packet(id + 1, 100), &shared, 7, 100)
            .is_none());
        let (_, bytes) = sender.prepare(packet(id, 100), &shared, 7, 100).unwrap();
        assert!(AudioHeader::decode(&bytes).unwrap().0.discontinuity);
        sender.last_stream = Some((id, 7));
        assert!(
            !AudioHeader::decode(&sender.prepare(packet(id, 100), &shared, 7, 100).unwrap().1)
                .unwrap()
                .0
                .discontinuity
        );
        assert!(
            AudioHeader::decode(&sender.prepare(packet(id, 100), &shared, 8, 100).unwrap().1)
                .unwrap()
                .0
                .discontinuity
        );
        assert!(sender
            .prepare(packet(id, 100), &shared, 7, 100_101)
            .is_none());
        assert!(
            AudioHeader::decode(
                &sender
                    .prepare(packet(id, 100_101), &shared, 7, 100_101)
                    .unwrap()
                    .1
            )
            .unwrap()
            .0
            .discontinuity
        );
    }
}
