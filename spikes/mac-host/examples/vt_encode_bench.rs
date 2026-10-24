//! Spike a: FFmpeg VideoToolbox encode bench, opening the encoder the way
//! `host/src/encoder/mod.rs` `EncoderState::new` does, then overriding the
//! options under test. Synthetic frames, no capture, no permissions.
//!
//! cargo run --release -p eternal-mac-spikes --example vt_encode_bench -- \
//!   --codec hevc --size inner --fps 120 --mode paced --seconds 20
//!
//! Modes: `throughput` (submit as fast as possible), `paced` (one frame per
//! interval, packets polled between frames), `reopen` (paced, reopening the
//! encoder at a new bitrate every few seconds, as the host does on an ABR
//! step). `--drain host` polls only right after each send, like the host's
//! encode loop.

fn main() {
    #[cfg(target_os = "macos")]
    imp::main();
}

#[cfg(target_os = "macos")]
#[path = "common/mod.rs"]
mod common;

#[cfg(target_os = "macos")]
mod imp {
    use super::common::{self, bitstream, vt, Args};
    use ffmpeg_next::format::Pixel;
    use std::collections::{HashMap, HashSet};
    use std::time::{Duration, Instant};

    struct Config {
        name: &'static str,
        hevc: bool,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        pix: Pixel,
        gop: i32,
        low_delay: bool,
        /// `None` leaves the colour range unset, as the host does.
        range: Option<ffmpeg_next::color::Range>,
        /// Set `ExpectedFrameRate` on FFmpeg's own session after open,
        /// which FFmpeg 7.1 never does.
        expected_fps: bool,
        /// Options applied after the host's own `encoder_options`.
        overrides: Vec<(String, String)>,
    }

    /// The host's `encoder_options` for the two VideoToolbox encoders:
    /// `h264_videotoolbox` falls through to the default arm.
    fn host_options(name: &str) -> Vec<(String, String)> {
        match name {
            "hevc_videotoolbox" => vec![("realtime".into(), "1".into())],
            _ => vec![("profile".into(), "baseline".into())],
        }
    }

    fn open(
        cfg: &Config,
        bitrate: u32,
    ) -> Result<(ffmpeg_next::codec::encoder::video::Encoder, Duration), String> {
        let started = Instant::now();
        let codec = ffmpeg_next::encoder::find_by_name(cfg.name).ok_or("codec not found")?;
        let mut encoder = ffmpeg_next::codec::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| e.to_string())?;
        encoder.set_width(cfg.width);
        encoder.set_height(cfg.height);
        encoder.set_format(cfg.pix);
        encoder.set_time_base(ffmpeg_next::Rational(1, 1_000_000));
        encoder.set_frame_rate(Some(ffmpeg_next::Rational(cfg.fps as i32, 1)));
        encoder.set_max_b_frames(0);
        encoder.set_bit_rate(bitrate.max(500_000) as usize);
        encoder.set_gop(cfg.gop as u32);
        if let Some(range) = cfg.range {
            encoder.set_color_range(range);
        }
        if cfg.low_delay {
            let flags = unsafe { (*encoder.as_ptr()).flags } as u32;
            encoder.set_flags(
                ffmpeg_next::codec::Flags::from_bits_truncate(flags)
                    | ffmpeg_next::codec::Flags::LOW_DELAY,
            );
        }
        let mut opts = ffmpeg_next::Dictionary::new();
        let mut merged: Vec<(String, String)> = host_options(cfg.name);
        for (key, value) in &cfg.overrides {
            merged.retain(|(k, _)| k != key);
            merged.push((key.clone(), value.clone()));
        }
        for (key, value) in &merged {
            opts.set(key, value);
        }
        let opened = encoder.open_with(opts).map_err(|e| {
            let code: i32 = e.into();
            format!("open failed: code={code} {e}")
        })?;
        if cfg.expected_fps {
            use core_foundation::base::TCFType;
            let session = ffmpeg_vt_session(&opened).ok_or("no VT session to set")?;
            let fps = core_foundation::number::CFNumber::from(cfg.fps as i32);
            let status = unsafe {
                vt::VTSessionSetProperty(
                    session,
                    vt::kVTCompressionPropertyKey_ExpectedFrameRate,
                    fps.as_CFTypeRef(),
                )
            };
            println!("set ExpectedFrameRate on FFmpeg's session status={status}");
        }
        Ok((opened, started.elapsed()))
    }

    /// FFmpeg 7.1's `VTEncContext` starts with `AVClass *class; enum
    /// AVCodecID codec_id; VTCompressionSessionRef session;`. The type check
    /// guards against a layout change.
    fn ffmpeg_vt_session(
        encoder: &ffmpeg_next::codec::encoder::video::Encoder,
    ) -> Option<*mut std::ffi::c_void> {
        unsafe {
            let private = (*encoder.as_ptr()).priv_data as *const *mut std::ffi::c_void;
            if private.is_null() {
                return None;
            }
            let session = *private.add(2);
            (!session.is_null() && vt::CFGetTypeID(session) == vt::VTCompressionSessionGetTypeID())
                .then_some(session)
        }
    }

    fn make_frames(cfg: &Config, count: usize) -> Vec<ffmpeg_next::frame::Video> {
        (0..count)
            .map(|n| {
                let mut frame = ffmpeg_next::frame::Video::new(cfg.pix, cfg.width, cfg.height);
                let (w, h) = (cfg.width as usize, cfg.height as usize);
                match cfg.pix {
                    Pixel::NV12 => {
                        let stride = frame.stride(0);
                        common::fill_luma(frame.data_mut(0), stride, w, h, n);
                        let stride = frame.stride(1);
                        common::fill_chroma(frame.data_mut(1), stride, w, h / 2, n);
                    }
                    Pixel::YUV420P => {
                        let stride = frame.stride(0);
                        common::fill_luma(frame.data_mut(0), stride, w, h, n);
                        for plane in 1..3 {
                            let stride = frame.stride(plane);
                            common::fill_chroma(frame.data_mut(plane), stride, w / 2, h / 2, n);
                        }
                    }
                    Pixel::BGRA => {
                        let stride = frame.stride(0);
                        common::fill_bgra(frame.data_mut(0), stride, w, h, n);
                    }
                    other => panic!("unsupported pixel format {other:?}"),
                }
                frame
            })
            .collect()
    }

    #[derive(Default)]
    struct Window {
        packets: u64,
        latencies: Vec<u64>,
        late_sends: u64,
    }

    struct Run {
        submit: HashMap<i64, Instant>,
        keyframe_requests: HashSet<i64>,
        latencies: Vec<u64>,
        keyframe_latencies: Vec<u64>,
        keyframe_misses: u64,
        send_us: Vec<u64>,
        packets: u64,
        keyframes: u64,
        bytes: u64,
        key_bytes: u64,
        first_key: Option<Vec<u8>>,
        first_packet_at: Option<Instant>,
        last_packet_at: Option<Instant>,
        packet_times: Vec<Instant>,
        same_call: u64,
        window: Window,
    }

    impl Run {
        fn new() -> Self {
            Self {
                submit: HashMap::new(),
                keyframe_requests: HashSet::new(),
                latencies: Vec::new(),
                keyframe_latencies: Vec::new(),
                keyframe_misses: 0,
                send_us: Vec::new(),
                packets: 0,
                keyframes: 0,
                bytes: 0,
                key_bytes: 0,
                first_key: None,
                first_packet_at: None,
                last_packet_at: None,
                packet_times: Vec::new(),
                same_call: 0,
                window: Window::default(),
            }
        }

        /// Drains every packet available now. Returns how many came out.
        fn drain(
            &mut self,
            encoder: &mut ffmpeg_next::codec::encoder::video::Encoder,
            packet: &mut ffmpeg_next::Packet,
        ) -> u64 {
            let mut drained = 0;
            while encoder.receive_packet(packet).is_ok() {
                drained += 1;
                self.record(packet);
            }
            drained
        }

        fn record(&mut self, packet: &ffmpeg_next::Packet) {
            {
                let now = Instant::now();
                self.packets += 1;
                self.window.packets += 1;
                let size = packet.size() as u64;
                self.bytes += size;
                let pts = packet.pts().unwrap_or(-1);
                let key = packet.is_key();
                if key {
                    self.keyframes += 1;
                    self.key_bytes += size;
                    if self.first_key.is_none() {
                        self.first_key = packet.data().map(<[u8]>::to_vec);
                    }
                }
                if let Some(at) = self.submit.remove(&pts) {
                    let us = (now - at).as_micros() as u64;
                    self.latencies.push(us);
                    self.window.latencies.push(us);
                    if self.keyframe_requests.remove(&pts) {
                        if key {
                            self.keyframe_latencies.push(us);
                        } else {
                            self.keyframe_misses += 1;
                        }
                    }
                }
                self.first_packet_at.get_or_insert(now);
                self.last_packet_at = Some(now);
                self.packet_times.push(now);
            }
        }

        fn send(
            &mut self,
            encoder: &mut ffmpeg_next::codec::encoder::video::Encoder,
            packet: &mut ffmpeg_next::Packet,
            frame: &mut ffmpeg_next::frame::Video,
            pts: i64,
            keyframe: bool,
        ) {
            frame.set_pts(Some(pts));
            frame.set_kind(if keyframe {
                ffmpeg_next::util::picture::Type::I
            } else {
                ffmpeg_next::util::picture::Type::None
            });
            unsafe { (*frame.as_mut_ptr()).key_frame = i32::from(keyframe) };
            if keyframe {
                self.keyframe_requests.insert(pts);
            }
            let started = Instant::now();
            self.submit.insert(pts, started);
            loop {
                match encoder.send_frame(frame) {
                    Ok(()) => break,
                    Err(ffmpeg_next::Error::Other { errno })
                        if errno == ffmpeg_next::util::error::EAGAIN =>
                    {
                        self.drain(encoder, packet);
                    }
                    Err(e) => panic!("send_frame: {e}"),
                }
            }
            self.send_us.push(started.elapsed().as_micros() as u64);
        }
    }

    fn print_session(label: &str, encoder: &ffmpeg_next::codec::encoder::video::Encoder) {
        match ffmpeg_vt_session(encoder) {
            Some(session) => {
                for line in vt::describe_session(session) {
                    println!("{label} session {line}");
                }
            }
            None => println!("{label} session <not a VTCompressionSession>"),
        }
    }

    fn print_bitstream(cfg: &Config, run: &Run) {
        if let Some(key) = &run.first_key {
            for pl in bitstream::describe_annex_b(key, cfg.hevc) {
                println!(
                    "bitstream {} profile=\"{}\" level={} level_idc={}",
                    pl.nal, pl.profile, pl.level, pl.level_idc
                );
            }
        }
        let (name, idc, rate) = if cfg.hevc {
            bitstream::hevc_min_level(cfg.width, cfg.height, cfg.fps)
        } else {
            bitstream::h264_min_level(cfg.width, cfg.height, cfg.fps)
        };
        println!(
            "bitstream required_level={name} required_idc={idc} rate={rate}{}",
            if cfg.hevc { " luma/s" } else { " MB/s" }
        );
    }

    pub fn main() {
        let args = Args::from_env();
        ffmpeg_next::init().unwrap();
        if args.flag("info") {
            unsafe {
                let c = std::ffi::CStr::from_ptr(ffmpeg_next::ffi::avcodec_configuration());
                let v = ffmpeg_next::ffi::avcodec_version();
                println!("libavcodec {}.{}.{}", v >> 16, (v >> 8) & 0xff, v & 0xff);
                println!("configuration: {}", c.to_string_lossy());
            }
            return;
        }
        let hevc = args.or("codec", "hevc") == "hevc";
        let (width, height) = common::parse_size(args.or("size", "inner"));
        let pix = match args.or("pix", "nv12") {
            "nv12" => Pixel::NV12,
            "bgra" => Pixel::BGRA,
            "yuv420p" => Pixel::YUV420P,
            other => panic!("pix {other}"),
        };
        let mut overrides = Vec::new();
        for key in [
            "realtime",
            "prio_speed",
            "profile",
            "level",
            "allow_sw",
            "require_sw",
            "power_efficient",
            "constant_bit_rate",
            "coder",
        ] {
            if let Some(value) = args.get(key) {
                overrides.push((key.to_string(), value.to_string()));
            }
        }
        let cfg = Config {
            name: if hevc {
                "hevc_videotoolbox"
            } else {
                "h264_videotoolbox"
            },
            hevc,
            width,
            height,
            fps: args.num("fps", 120),
            bitrate: (args.num("mbps", 15.0f64) * 1e6) as u32,
            pix,
            gop: args.num("gop", 30),
            low_delay: args.or("low_delay", "0") == "1",
            expected_fps: args.or("expected_fps", "0") == "1",
            range: match args.get("range") {
                Some("mpeg") => Some(ffmpeg_next::color::Range::MPEG),
                Some("jpeg") => Some(ffmpeg_next::color::Range::JPEG),
                None => None,
                Some(other) => panic!("range {other}"),
            },
            overrides,
        };
        let mode = args.or("mode", "paced").to_string();
        let seconds: f64 = args.num("seconds", 10.0);
        let label = args.or("label", "").to_string();
        let keyframe_every: f64 = args.num("keyframe_every", 0.0);
        let window_s: f64 = args.num("window", 30.0);
        let host_drain = args.or("drain", "poll") == "host";
        let reopen_every: f64 = args.num("reopen_every", 2.0);

        println!(
            "config label={label} encoder={} size={}x{} fps={} mbps={} pix={:?} range={:?} gop={} low_delay={} overrides={:?} mode={mode} seconds={seconds} drain={}",
            cfg.name,
            cfg.width,
            cfg.height,
            cfg.fps,
            cfg.bitrate as f64 / 1e6,
            cfg.pix,
            cfg.range,
            cfg.gop,
            cfg.low_delay,
            cfg.overrides,
            if host_drain { "host" } else { "poll" }
        );
        println!("state start {}", common::machine_state());

        let (mut encoder, open_time) = match open(&cfg, cfg.bitrate) {
            Ok(opened) => opened,
            Err(e) => {
                println!("RESULT label={label} status=open_failed error=\"{e}\"");
                return;
            }
        };
        println!("open_ms={:.1}", open_time.as_secs_f64() * 1000.0);
        print_session("open", &encoder);

        let mut frames = make_frames(&cfg, 8);
        let mut packet = ffmpeg_next::Packet::empty();
        let mut run = Run::new();
        let interval = Duration::from_secs_f64(1.0 / f64::from(cfg.fps));
        let total_frames = (seconds * f64::from(cfg.fps)).round() as u64;
        let keyframe_period = (keyframe_every * f64::from(cfg.fps)).round() as u64;
        let reopen_period = (reopen_every * f64::from(cfg.fps)).round() as u64;
        let window_frames = ((window_s * f64::from(cfg.fps)).round() as u64).max(1);
        let mut reopen_stats: Vec<String> = Vec::new();
        let mut reopen_bitrate = cfg.bitrate;
        let mut dropped_in_flight = 0u64;
        let mut pending_gap: Option<Instant> = None;
        let mut gaps_us = Vec::new();
        let mut late_sends = 0u64;
        let mut session_frame = 0u64;

        let started = Instant::now();
        let mut window_started = started;
        for i in 0..total_frames {
            let deadline = started + interval * i as u32;
            if mode != "throughput" {
                if host_drain {
                    common::sleep_until(deadline);
                } else {
                    while Instant::now() < deadline {
                        if run.drain(&mut encoder, &mut packet) == 0 {
                            let left = deadline.saturating_duration_since(Instant::now());
                            if left > Duration::from_micros(400) {
                                std::thread::sleep(Duration::from_micros(200));
                            }
                        } else if let Some(at) = pending_gap.take() {
                            gaps_us.push((Instant::now() - at).as_micros() as u64);
                        }
                    }
                }
                if Instant::now() > deadline + interval {
                    late_sends += 1;
                    run.window.late_sends += 1;
                }
            }

            let mut force_key = keyframe_period > 0 && i > 0 && i % keyframe_period == 0;
            if mode == "reopen" && i > 0 && i % reopen_period == 0 {
                reopen_bitrate = if reopen_bitrate == cfg.bitrate {
                    cfg.bitrate * 4 / 3
                } else {
                    cfg.bitrate
                };
                let before = Instant::now();
                let (fresh, open_time) = open(&cfg, reopen_bitrate).expect("reopen");
                let old = std::mem::replace(&mut encoder, fresh);
                let drop_started = Instant::now();
                drop(old);
                let drop_time = drop_started.elapsed();
                dropped_in_flight += run.submit.len() as u64;
                run.submit.clear();
                run.keyframe_requests.clear();
                pending_gap = run.last_packet_at;
                reopen_stats.push(format!(
                    "{:.1}/{:.1}/{:.1}",
                    open_time.as_secs_f64() * 1000.0,
                    drop_time.as_secs_f64() * 1000.0,
                    before.elapsed().as_secs_f64() * 1000.0
                ));
                session_frame = 0;
                force_key = true;
            }
            let pts = (i as f64 * 1e6 / f64::from(cfg.fps)) as i64;
            let frame = &mut frames[(i % 8) as usize];
            run.send(
                &mut encoder,
                &mut packet,
                frame,
                pts,
                force_key && session_frame > 0,
            );
            session_frame += 1;
            if run.drain(&mut encoder, &mut packet) > 0 {
                run.same_call += 1;
                if let Some(at) = pending_gap.take() {
                    gaps_us.push((Instant::now() - at).as_micros() as u64);
                }
            }

            if mode != "throughput" && (i + 1) % window_frames == 0 {
                let elapsed = window_started.elapsed().as_secs_f64();
                let w = std::mem::take(&mut run.window);
                println!(
                    "window t={:.0}s out_fps={:.1} {} late_sends={} in_flight={} {}",
                    started.elapsed().as_secs_f64(),
                    w.packets as f64 / elapsed,
                    common::summarize(w.latencies),
                    w.late_sends,
                    run.submit.len(),
                    common::machine_state()
                );
                window_started = Instant::now();
            }
        }
        let submitted_all = Instant::now();
        encoder.send_eof().ok();
        loop {
            match encoder.receive_packet(&mut packet) {
                Ok(()) => run.record(&packet),
                Err(ffmpeg_next::Error::Eof) => break,
                Err(_) => std::thread::sleep(Duration::from_micros(100)),
            }
        }
        let wall = started.elapsed().as_secs_f64();

        // Steady-state output rate: packets after the first 20%, timed by
        // their arrival.
        let steady_fps = {
            let n = run.packet_times.len();
            let from = n / 5;
            if n > from + 2 {
                (n - from - 1) as f64
                    / (run.packet_times[n - 1] - run.packet_times[from]).as_secs_f64()
            } else {
                f64::NAN
            }
        };
        print_session("end", &encoder);
        print_bitstream(&cfg, &run);
        println!("state end {}", common::machine_state());
        let latency = common::summarize(run.latencies.clone());
        let send = common::summarize(run.send_us.clone());
        let key = common::summarize(run.keyframe_latencies.clone());
        let achieved_mbps = run.bytes as f64 * 8.0 / wall / 1e6;
        println!(
            "RESULT label={label} status=ok encoder={} size={}x{} fps_target={} mode={mode} frames={} packets={} wall_s={:.2} throughput_fps={:.1} steady_fps={:.1} submit_only_s={:.2} latency[{latency}] send[{send}] keyframe_req[{key}] keyframe_misses={} keyframes={} achieved_mbps={:.2} key_kb_avg={:.1} same_call_pct={:.1} late_sends={late_sends}",
            cfg.name,
            cfg.width,
            cfg.height,
            cfg.fps,
            total_frames,
            run.packets,
            wall,
            run.packets as f64 / wall,
            steady_fps,
            (submitted_all - started).as_secs_f64(),
            run.keyframe_misses,
            run.keyframes,
            achieved_mbps,
            if run.keyframes > 0 {
                run.key_bytes as f64 / run.keyframes as f64 / 1024.0
            } else {
                0.0
            },
            100.0 * run.same_call as f64 / total_frames.max(1) as f64,
        );
        if mode == "reopen" {
            println!(
                "REOPEN label={label} count={} open/drop/total_ms={} dropped_in_flight={dropped_in_flight} gap[{}]",
                reopen_stats.len(),
                reopen_stats.join(","),
                common::summarize(gaps_us)
            );
        }
    }
}
