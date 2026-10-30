// Frame-by-frame footage of the real Windows app's interface for the home page films.
//
// capture-host.sh appends this file to a scratch copy of host/tests/gui_snapshots.rs
// (it reuses that file's fixture, harness and iPad helpers) and runs it there, so
// nothing in host/ changes. Each film is 240 frames at 30 fps, drawn by the app
// itself through egui_kittest at 2x (the performance sessions are 96 frames
// each, so the film can slide between them). The pointer path is written next to the
// frames as pointer.json, so the film can draw a cursor exactly where the app
// saw the pointer.

struct Film {
    dir: std::path::PathBuf,
    frame: usize,
    pointer: egui::Pos2,
    log: Vec<String>,
}

impl Film {
    fn new(root: &std::path::Path, name: &str, start: egui::Pos2) -> Self {
        let dir = root.join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir, frame: 0, pointer: start, log: Vec::new() }
    }

    /// Draws one frame with the pointer where it is now.
    fn shoot(&mut self, h: &mut Harness<'_, AnalyzerApp>, down: bool) {
        h.hover_at(self.pointer);
        h.step();
        let image = h.render().expect("render");
        image.save(self.dir.join(format!("{:04}.png", self.frame))).unwrap();
        self.log.push(format!(
            "[{:.1},{:.1},{}]",
            self.pointer.x, self.pointer.y, if down { 1 } else { 0 }
        ));
        self.frame += 1;
    }

    fn hold(&mut self, h: &mut Harness<'_, AnalyzerApp>, frames: usize) {
        for _ in 0..frames {
            self.shoot(h, false);
        }
    }

    /// Glides the pointer to `to` over `frames` frames, easing in and out.
    fn glide(&mut self, h: &mut Harness<'_, AnalyzerApp>, to: egui::Pos2, frames: usize) {
        let from = self.pointer;
        for i in 1..=frames {
            let t = i as f32 / frames as f32;
            let e = t * t * (3.0 - 2.0 * t);
            self.pointer = from + (to - from) * e;
            self.shoot(h, false);
        }
    }

    /// A press and release where the pointer is, over two frames.
    fn click(&mut self, h: &mut Harness<'_, AnalyzerApp>) {
        let pos = self.pointer;
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        h.event(button(true));
        self.shoot(h, true);
        h.event(button(false));
        self.shoot(h, true);
    }

    fn finish(self) {
        self.finish_at(240);
    }

    fn finish_at(self, frames: usize) {
        assert_eq!(self.frame, frames, "{} has {} frames", self.dir.display(), self.frame);
        std::fs::write(self.dir.join("pointer.json"), format!("[{}]\n", self.log.join(","))).unwrap();
    }
}

fn film_harness(shared: &SharedControl, size: egui::Vec2) -> Harness<'static, AnalyzerApp> {
    let (supervisor_tx, _supervisor_rx) = std::sync::mpsc::channel();
    let control = GuiControl { shared: shared.clone(), supervisor_tx };
    let mut h = Harness::builder()
        .with_size(size)
        .with_pixels_per_point(2.0)
        .with_step_dt(1.0 / 30.0)
        .with_theme(egui::Theme::Dark)
        .wgpu()
        .build_eframe(|cc| AnalyzerApp::new(cc, control));
    h.run_steps(8);
    h
}

fn centre(h: &Harness<'_, AnalyzerApp>, role: Option<Role>, label: &str) -> egui::Pos2 {
    match role {
        Some(role) => h.get_by_role_and_label(role, label).rect().center(),
        None => h.get_by_label(label).rect().center(),
    }
}

/// Records where the film's overlays go, in the frame's 1x points.
fn note(film: &Film, name: &str, rect: egui::Rect) {
    let line = format!(
        "{name} {:.1} {:.1} {:.1} {:.1}\n",
        rect.left(), rect.top(), rect.width(), rect.height()
    );
    let path = film.dir.join("rects.txt");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    text.push_str(&line);
    std::fs::write(path, text).unwrap();
}

/// A small seeded generator, so every capture draws the same trace.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Roughly normal, mean 0 and spread 1.
    fn normal(&mut self) -> f64 {
        (0..4).map(|_| self.next()).sum::<f64>() - 2.0
    }
}

/// An encode-time trace in milliseconds as a GPU encoder draws it: a level
/// that wanders a little, frame-to-frame jitter, and a keyframe or a busy
/// scene now and then, at no fixed interval.
fn encode_trace(seed: u64, base: f64, len: usize) -> Vec<f64> {
    let mut rng = Rng(seed);
    let mut wander = 0.0;
    let mut jitter = 0.0;
    let mut next_spike = 20 + (rng.next() * 60.0) as usize;
    let mut tail = 0.0;
    (0..len)
        .map(|i| {
            wander = 0.985 * wander + 0.015 * rng.normal() * base * 0.9;
            jitter = 0.55 * jitter + 0.45 * rng.normal() * base * 0.07;
            let mut v = base + wander + jitter;
            if i == next_spike {
                // A keyframe costs one to two and a half times the usual frame.
                let size = if rng.next() < 0.2 { 2.2 + rng.next() * 0.5 } else { 0.9 + rng.next() * 0.7 };
                v += base * size;
                tail = base * size * (0.15 + rng.next() * 0.2);
                next_spike = i + 35 + (rng.next() * 110.0) as usize;
            } else if tail > 0.01 {
                v += tail;
                tail *= 0.35;
            }
            v.max(base * 0.55)
        })
        .collect()
}

/// One streaming session on one PC, for the Performance film.
struct Session {
    film: &'static str,
    gpu: &'static str,
    codec: &'static str,
    seed: u64,
    encode_ms: f64,
    uptime_s: u64,
    frames: u64,
    bytes: u64,
    packets: u64,
    retransmits: u64,
    frame_bytes: usize,
}

/// The iPad that scanned the QR code joins over Wi-Fi, so the app says so.
fn wifi_peer() -> PeerId {
    PeerId::udp("192.0.2.40:9877".parse().unwrap())
}

fn connect_wifi(shared: &SharedControl) {
    let now = Instant::now();
    let mut session = shared.session.lock();
    let hello = Hello2 {
        proto_min: 2,
        proto_max: 2,
        client_nonce: 31,
        listen_port: 9877,
        decoder_caps: 3,
        feature_caps: 3 | FEATURE_CHECKS_UPDATES,
        screen_px_w: 2420,
        screen_px_h: 1668,
        screen_pt_w: 1210,
        screen_pt_h: 834,
        refresh_hz: 120,
        device_name: "iPad Pro".into(),
        device_id: 9,
        preferred_fps: 60,
        auth_token: [0x22; 16],
        pairing_code: 0,
    };
    let actions = session.handle_control(wifi_peer(), 0, ControlMessage::Hello2(hello), &Fixture, now);
    assert_eq!(actions.new_target, Some(wifi_peer()));
    let session_id = session.session_id().unwrap();
    session.handle_control(
        wifi_peer(),
        session_id,
        ControlMessage::ReceiverReport(ReceiverReport {
            stream_epoch: 1,
            highest_seq: 1200,
            frames_complete: 1200,
            frames_dropped: 1,
            frags_received: 970,
            frags_lost: 10,
            frags_repaired: 20,
            jitter_us: 450,
            decode_fps_x10: 599,
            assembler_depth: 0,
            decode_depth: 0,
            e2e_latency_ms_x10: 184,
            rtt_ms_x10: 32,
            nacks_sent: 10,
            audio_packets_lost: 0,
            audio_buffer_ms: 60,
        }),
        &Fixture,
        now,
    );
}

fn disconnect_wifi(shared: &SharedControl) {
    let mut session = shared.session.lock();
    let session_id = session.session_id().unwrap();
    session.handle_control(wifi_peer(), session_id, ControlMessage::Bye(ByeReason::UserDisconnect), &Fixture, Instant::now());
}

#[test]
#[ignore = "writes film frames; run through site-demos/capture/capture-host.sh"]
fn home_page_films() {
    let root = std::path::PathBuf::from(std::env::var("EM_FILM_DIR").expect("EM_FILM_DIR"));
    let only = std::env::var("EM_FILM_ONLY").unwrap_or_default();
    let wants = |name: &str| only.is_empty() || only.split(',').any(|n| n == name);
    let size = egui::vec2(1040.0, 720.0);

    // Pair: the QR code opens at 1.2 s; the iPad that scans it joins at 4.9 s
    // and Done closes the dialog at 5.6 s. The iPad leaves as the film loops.
    if wants("pair") {
        let (_dir, shared) = fixture();
        let mut h = film_harness(&shared, size);
        let mut f = Film::new(&root, "pair", egui::pos2(760.0, 600.0));
        let qr = centre(&h, Some(Role::Button), "QR code");
        f.hold(&mut h, 6);
        f.glide(&mut h, qr, 26);
        f.hold(&mut h, 3);
        // The dialog's address is a documentation one from its first frame.
        PIPELINE_STATS.lock().listen_addr = "192.0.2.10:9876".into();
        f.click(&mut h);
        f.hold(&mut h, 16);
        note(&f, "dialog", h.get_by_label("Scan to connect").rect());
        f.glide(&mut h, egui::pos2(880.0, 610.0), 24);
        let used = f.frame;
        f.hold(&mut h, 147 - used);
        connect_wifi(&shared);
        f.hold(&mut h, 4);
        if let Some(done) = h.query_by_label("Done") {
            let done = done.rect().center();
            f.glide(&mut h, done, 12);
            f.hold(&mut h, 2);
            f.click(&mut h);
        }
        let used = f.frame;
        f.hold(&mut h, 212 - used);
        f.glide(&mut h, egui::pos2(760.0, 600.0), 22);
        disconnect_wifi(&shared);
        f.hold(&mut h, 6);
        f.finish();
    }

    // Second screen: from Mirror my screen to Second screen, which it keeps.
    // The click lands at 1.5 s; the pointer then leaves the window on the right
    // at 2.2 s, where the film's own pointer carries a document to the iPad.
    if wants("second-screen") {
        let (_dir, shared) = fixture();
        connect_named(&shared, "iPad Pro", 10, 20);
        let mut h = film_harness(&shared, size);
        let mut f = Film::new(&root, "second-screen", egui::pos2(760.0, 420.0));
        let second = centre(&h, None, "Second screen");
        note(&f, "choice", h.get_by_label("Use the iPad as").rect());
        f.hold(&mut h, 12);
        f.glide(&mut h, second, 28);
        f.hold(&mut h, 4);
        f.click(&mut h);
        f.hold(&mut h, 10);
        *shared.vdd_status.lock() = VddStatus::Active;
        f.glide(&mut h, egui::pos2(1080.0, 300.0), 22);
        let used = f.frame;
        f.hold(&mut h, 240 - used);
        f.finish();
    }

    // Performance: three PCs, each with its own GPU and its own session. The
    // film slides from one to the next, so each is 96 frames: 3.2 s, enough to
    // overlap the slides on either side.
    let sessions = [
        Session { film: "performance-nvidia", gpu: "NVIDIA GeForce RTX 4070", codec: "h264_nvenc", seed: 0x9e37_79b9_7f4a_7c15, encode_ms: 1.05, uptime_s: 1_513, frames: 90_780, bytes: 2_838_000_000, packets: 2_053_350, retransmits: 212, frame_bytes: 31_200 },
        Session { film: "performance-amd", gpu: "AMD Radeon RX 7800 XT", codec: "hevc_amf", seed: 0x2545_f491_4f6c_dd1d, encode_ms: 2.3, uptime_s: 4_327, frames: 259_560, bytes: 8_112_000_000, packets: 5_871_204, retransmits: 1_046, frame_bytes: 29_400 },
        Session { film: "performance-intel", gpu: "Intel Arc A750", codec: "h264_qsv", seed: 0x6a09_e667_f3bc_c908, encode_ms: 1.6, uptime_s: 468, frames: 28_032, bytes: 877_000_000, packets: 634_880, retransmits: 37, frame_bytes: 32_600 },
    ];
    for session in &sessions {
        if !wants("performance") && !wants(session.film) {
            continue;
        }
        const LEN: usize = 96;
        let (_dir, shared) = fixture();
        connect_named(&shared, "iPad Pro", 10, 20);
        // 300 samples on screen, two encoded frames for every film frame.
        let trace = encode_trace(session.seed, session.encode_ms, 300 + 2 * LEN);
        let mut size = Rng(session.seed ^ 0xff);
        let feed = |frame: usize, size: &mut Rng| {
            let mut stats = PIPELINE_STATS.lock();
            stats.gpu_name = session.gpu.into();
            stats.codec_name = session.codec.into();
            stats.encode_time_history = trace[frame * 2..frame * 2 + 300].iter().copied().collect();
            stats.encode_time_us = (trace[frame * 2 + 299] * 1000.0) as u128;
            // At 60 fps and 15 Mbps. Frames are drawn slower than real time,
            // so uptime is set, not measured.
            let n = frame as u64;
            stats.start_time = Some(
                std::time::Instant::now()
                    - std::time::Duration::from_millis(session.uptime_s * 1000 + n * 1000 / 30),
            );
            stats.capture_frame_count = session.frames + n * 2;
            stats.encode_frame_count = session.frames - 2 + n * 2;
            stats.nal_bytes_last = (session.frame_bytes as f64 * (0.9 + 0.2 * size.next())) as usize;
            stats.transport_bytes_sent = session.bytes + n * 62_500;
            stats.transport_packets_sent = session.packets + n * 45;
            stats.transport_fragments_sent = session.packets + n * 45;
            stats.transport_retransmits = session.retransmits;
        };
        feed(0, &mut size);
        let mut h = film_harness(&shared, egui::vec2(1040.0, 1000.0));
        nav(&mut h, "Performance");
        let mut f = Film::new(&root, session.film, egui::pos2(-50.0, -50.0));
        note(&f, "gpu", h.get_by_label("GPU").rect());
        note(&f, "chart", h.get_by_label("Encode time").rect());
        for frame in 0..LEN {
            feed(frame, &mut size);
            f.shoot(&mut h, false);
        }
        f.finish_at(LEN);
    }

    // Feedback: open the panel from the sidebar, write a line, send it to the email app.
    if wants("feedback") {
        let (_dir, shared) = fixture();
        connect_named(&shared, "iPad Pro", 10, 20);
        let mut h = film_harness(&shared, size);
        nav(&mut h, "Settings");
        h.run_steps(8);
        let mut f = Film::new(&root, "feedback", egui::pos2(520.0, 520.0));
        let open = h
            .get_all_by_role_and_label(Role::Button, "Send feedback")
            .find(|node| node.rect().left() < 224.0)
            .expect("the sidebar button")
            .rect()
            .center();
        f.hold(&mut h, 12);
        f.glide(&mut h, open, 28);
        f.hold(&mut h, 3);
        f.click(&mut h);
        f.hold(&mut h, 12);
        let like = centre(&h, None, "Something I like");
        f.glide(&mut h, like, 18);
        f.click(&mut h);
        f.hold(&mut h, 4);
        let field = h.get_by_role(Role::MultilineTextInput).rect();
        note(&f, "panel", h.get_by_label("Tell us how it's going").rect());
        f.glide(&mut h, field.center(), 14);
        f.click(&mut h);
        // The pointer steps aside while the words go in, about 13 letters a second.
        f.glide(&mut h, field.right_bottom() + egui::vec2(-60.0, 40.0), 10);
        let words = "Drawing in Krita feels great.";
        for (i, ch) in words.chars().enumerate() {
            h.event(egui::Event::Text(ch.to_string()));
            f.shoot(&mut h, false);
            if i % 2 == 1 {
                f.shoot(&mut h, false);
            }
        }
        // Open in my email app at 5.8 s; the app's own note says what happens.
        let send = centre(&h, Some(Role::Button), "Open in my email app");
        f.glide(&mut h, send, 18);
        f.hold(&mut h, 4);
        f.click(&mut h);
        f.glide(&mut h, send + egui::vec2(40.0, 70.0), 14);
        let used = f.frame;
        assert!(used <= 200, "feedback runs long: {used}");
        f.hold(&mut h, 240 - used);
        f.finish();
    }
}
