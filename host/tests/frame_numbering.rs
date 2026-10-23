//! The wire's `frame_seq` counts the access units sent in a stream epoch with
//! no gaps, as `proto` documents. Capture numbers are not contiguous: the
//! encoder takes only the latest captured frame, and 14% of captured frames
//! were never encoded while streaming. Each skipped number made the iPad hold
//! the next frame for half a frame period, waiting for one that never existed.

use std::net::UdpSocket;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use eternal_host::control::SharedControl;
use eternal_host::encoder::NALUnit;
use eternal_wire::v2::control::{
    encode_control, parse_control, ControlMessage, Hello2, Nack, CAP_DECODE_H264,
    FEATURE_SUPPORTS_NACK, HOSTCAP_CONTIGUOUS_FRAME_SEQ,
};
use eternal_wire::v2::media::MediaHeader;

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map(|a| a.port())
        .unwrap()
}

#[test]
fn skipped_capture_frames_leave_no_gap_on_the_wire() {
    let port = free_udp_port();
    let shared = SharedControl::new(port, 15_000_000);
    shared.pairing.lock().required = false;
    // The supervisor normally marks the pipeline running.
    shared
        .running
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (nal_tx, nal_rx) = tokio::sync::mpsc::channel(16);
    let (_audio_tx, audio_rx) = tokio::sync::mpsc::channel(4);
    let (supervisor_tx, _supervisor_rx) = std_mpsc::channel();
    let sender_shared = shared.clone();
    let sender = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(eternal_host::transport::start_sender(
                nal_rx,
                audio_rx,
                port,
                sender_shared,
                supervisor_tx,
            ))
            .unwrap();
    });

    let ipad = UdpSocket::bind("127.0.0.1:0").unwrap();
    ipad.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let host = format!("127.0.0.1:{port}");
    let hello = encode_control(
        0,
        1,
        &ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: 7,
            listen_port: ipad.local_addr().unwrap().port(),
            decoder_caps: CAP_DECODE_H264,
            feature_caps: FEATURE_SUPPORTS_NACK,
            screen_px_w: 640,
            screen_px_h: 360,
            screen_pt_w: 640,
            screen_pt_h: 360,
            refresh_hz: 60,
            device_name: "Numbering iPad".into(),
            device_id: 0,
            preferred_fps: 0,
            auth_token: [0; 16],
            pairing_code: 0,
        }),
    );
    let mut buffer = [0; 2048];
    let deadline = Instant::now() + Duration::from_secs(10);
    let session_id = loop {
        assert!(Instant::now() < deadline, "no HELLO_ACK");
        ipad.send_to(&hello, &host).unwrap();
        if let Ok(len) = ipad.recv(&mut buffer) {
            if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(&buffer[..len]) {
                // The iPad may treat a gap as a lost frame only when told.
                assert_ne!(ack.host_caps & HOSTCAP_CONTIGUOUS_FRAME_SEQ, 0);
                break ack.session_id;
            }
        }
    };

    // Capture numbers with gaps, as the latest-wins encoder mailbox leaves them.
    let captured = [10u64, 11, 14, 15, 30, 31];
    for (n, &sequence) in captured.iter().enumerate() {
        nal_tx
            .blocking_send(NALUnit {
                data: vec![0, 0, 0, 1, if n == 0 { 0x65 } else { 0x41 }, n as u8],
                sequence,
                timestamp: Instant::now(),
                encode_duration_us: 0,
                is_keyframe: n == 0,
            })
            .unwrap();
    }
    let mut wire = Vec::new();
    let mut epoch = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while wire.len() < captured.len() && Instant::now() < deadline {
        if let Ok(len) = ipad.recv(&mut buffer) {
            if let Ok((header, payload)) = MediaHeader::decode(&buffer[..len]) {
                assert_eq!(header.session_id, session_id);
                epoch = header.stream_epoch;
                wire.push((header.frame_seq, payload[5]));
            }
        }
    }
    assert_eq!(
        wire,
        [(1, 0), (2, 1), (3, 2), (4, 3), (5, 4), (6, 5)],
        "frames are numbered 1, 2, 3... whatever their capture numbers"
    );

    // Repair uses the same numbers: a NACK for frame 4 resends frame 4.
    let nack = ControlMessage::Nack(Nack {
        stream_epoch: epoch,
        frame_seq: 4,
        frag_count: 1,
        missing: vec![0],
    });
    ipad.send_to(&encode_control(session_id, 3, &nack), &host)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut resent = None;
    while resent.is_none() && Instant::now() < deadline {
        if let Ok(len) = ipad.recv(&mut buffer) {
            if let Ok((header, payload)) = MediaHeader::decode(&buffer[..len]) {
                if header.is_retransmit {
                    resent = Some((header.frame_seq, payload[5]));
                }
            }
        }
    }
    assert_eq!(
        resent,
        Some((4, 3)),
        "frame 4 carries the fourth access unit"
    );

    drop(nal_tx);
    shared.stop();
    sender.join().unwrap();
}
