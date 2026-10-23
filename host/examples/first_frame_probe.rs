//! Connects to a running host as a minimal H.264 client and prints the colour
//! of one pixel in the first decoded frame, to check what a newly connecting
//! iPad sees first. Usage:
//! first_frame_probe <host:port> <x> <y> [frame.ppm|-] [preferred_fps]
//! Pairing must be off on the host. The optional path receives the first
//! frame, downscaled 4x, as a binary PPM. A preferred frame rate other than
//! the previous viewer's makes the host reopen its encoder for this viewer.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use eternal_wire::reassembly::{AddOutcome, Reassembler};
use eternal_wire::v2::control::{
    encode_control, parse_control, ControlMessage, Hello2, HelloStatus, ReceiverReport,
    CAP_DECODE_H264,
};
use eternal_wire::v2::media::MediaHeader;
use eternal_wire::v2::{classify, Classified};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    assert!(
        (3..=5).contains(&args.len()),
        "usage: first_frame_probe <host:port> <x> <y> [frame.ppm|-] [preferred_fps]"
    );
    let host = args[0].clone();
    let (x, y): (usize, usize) = (args[1].parse().unwrap(), args[2].parse().unwrap());
    let ppm = args.get(3).filter(|path| *path != "-");
    let preferred_fps: u8 = args.get(4).map_or(60, |fps| fps.parse().unwrap());
    ffmpeg_next::init().unwrap();
    ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Quiet);

    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let _ = socket2::SockRef::from(&socket).set_recv_buffer_size(8 * 1024 * 1024);
    let hello = encode_control(
        0,
        1,
        &ControlMessage::Hello2(Hello2 {
            proto_min: 2,
            proto_max: 2,
            client_nonce: 0xF1F5_7F00,
            listen_port: socket.local_addr().unwrap().port(),
            decoder_caps: CAP_DECODE_H264,
            feature_caps: 0,
            screen_px_w: 2420,
            screen_px_h: 1668,
            screen_pt_w: 1210,
            screen_pt_h: 834,
            refresh_hz: 60,
            device_name: "first-frame probe".into(),
            device_id: 0,
            preferred_fps,
            auth_token: [0; 16],
            pairing_code: 0,
        }),
    );

    let started = Instant::now();
    let mut session_id = 0;
    let mut msg_seq = 1;
    let mut last_send = started - Duration::from_secs(1);
    let mut acked_at = None;
    let mut reassembler = Reassembler::new();
    let codec = ffmpeg_next::decoder::find(ffmpeg_next::codec::Id::H264).unwrap();
    let mut decoder = ffmpeg_next::codec::Context::new_with_codec(codec)
        .decoder()
        .video()
        .unwrap();
    let mut frame = ffmpeg_next::frame::Video::empty();
    let mut buf = vec![0u8; 2048];
    // Like the iPad, decode nothing before the first keyframe: a P-frame
    // without its reference decodes to concealment, not the desktop.
    let mut synced = false;
    loop {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "no frame within 20 s"
        );
        if last_send.elapsed() >= Duration::from_millis(300) {
            last_send = Instant::now();
            let message = if session_id == 0 {
                hello.clone()
            } else {
                msg_seq += 1;
                encode_control(
                    session_id,
                    msg_seq,
                    &ControlMessage::ReceiverReport(ReceiverReport::default()),
                )
            };
            socket.send_to(&message, &host).unwrap();
        }
        let Ok((len, _)) = socket.recv_from(&mut buf) else {
            continue;
        };
        let bytes = &buf[..len];
        match classify(bytes) {
            Classified::Control(_) => {
                if let Ok((_, ControlMessage::HelloAck(ack))) = parse_control(bytes) {
                    assert_eq!(ack.status, HelloStatus::Ok, "pairing must be off");
                    session_id = ack.session_id;
                    acked_at.get_or_insert_with(Instant::now);
                }
            }
            Classified::Media { .. } if session_id != 0 => {
                let (header, payload) = MediaHeader::decode(bytes).unwrap();
                let is_keyframe = header.is_keyframe;
                let AddOutcome::Completed(access_unit) = reassembler.add_fragment(
                    header.frame_seq,
                    header.frag_index,
                    header.frag_count,
                    header.stream_epoch,
                    payload,
                    Instant::now(),
                ) else {
                    continue;
                };
                synced |= is_keyframe;
                if !synced {
                    continue;
                }
                if decoder
                    .send_packet(&ffmpeg_next::Packet::copy(&access_unit))
                    .is_err()
                {
                    continue;
                }
                if decoder.receive_frame(&mut frame).is_ok() {
                    let (r, g, b) = rgb_at(&frame, x, y);
                    if let Some(path) = ppm {
                        write_ppm(&frame, path);
                    }
                    println!(
                        "FIRST_FRAME w={} h={} x={x} y={y} rgb={r},{g},{b} ms_after_ack={}",
                        frame.width(),
                        frame.height(),
                        acked_at.map_or(0, |at| at.elapsed().as_millis())
                    );
                    return;
                }
            }
            _ => {}
        }
    }
}

/// BT.601 limited-range YUV 4:2:0 to RGB for one pixel.
fn rgb_at(frame: &ffmpeg_next::frame::Video, x: usize, y: usize) -> (u8, u8, u8) {
    let luma = f32::from(frame.data(0)[y * frame.stride(0) + x]);
    let u = f32::from(frame.data(1)[(y / 2) * frame.stride(1) + x / 2]) - 128.0;
    let v = f32::from(frame.data(2)[(y / 2) * frame.stride(2) + x / 2]) - 128.0;
    let c = 1.164 * (luma - 16.0);
    let clamp = |value: f32| value.round().clamp(0.0, 255.0) as u8;
    (
        clamp(c + 1.596 * v),
        clamp(c - 0.392 * u - 0.813 * v),
        clamp(c + 2.017 * u),
    )
}

fn write_ppm(frame: &ffmpeg_next::frame::Video, path: &str) {
    let (width, height) = (frame.width() as usize / 4, frame.height() as usize / 4);
    let mut bytes = format!("P6\n{width} {height}\n255\n").into_bytes();
    for y in 0..height {
        for x in 0..width {
            let (r, g, b) = rgb_at(frame, x * 4, y * 4);
            bytes.extend([r, g, b]);
        }
    }
    std::fs::write(path, bytes).unwrap();
}
