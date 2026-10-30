// Frame-by-frame footage of the real Windows app's interface for the home page films.
//
// capture-host.sh appends this file to a scratch copy of host/tests/gui_snapshots.rs
// (it reuses that file's fixture, harness and iPad helpers) and runs it there, so
// nothing in host/ changes. Each film is 240 frames at 30 fps, drawn by the app
// itself through egui_kittest at 2x. The pointer path is written next to the
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
        assert_eq!(self.frame, 240, "{} has {} frames", self.dir.display(), self.frame);
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

/// A periodic encode-time trace: one wave every 30 samples and a keyframe
/// spike every 60, so 480 samples later it is back where it started.
fn encode_sample(i: usize) -> f64 {
    let wave = 1.1 + 0.32 * ((i as f64) * std::f64::consts::TAU / 30.0).sin();
    let spike = match i % 60 {
        0 => 1.7,
        1 => 0.5,
        _ => 0.0,
    };
    wave + spike
}

#[test]
#[ignore = "writes film frames; run through site-demos/capture/capture-host.sh"]
fn home_page_films() {
    let root = std::path::PathBuf::from(std::env::var("EM_FILM_DIR").expect("EM_FILM_DIR"));
    let only = std::env::var("EM_FILM_ONLY").unwrap_or_default();
    let wants = |name: &str| only.is_empty() || only.split(',').any(|n| n == name);
    let size = egui::vec2(1040.0, 720.0);

    // Pair: the QR code, an iPad arrives, the dialog closes on its own or by Done.
    if wants("pair") {
        let (_dir, shared) = fixture();
        let mut h = film_harness(&shared, size);
        let mut f = Film::new(&root, "pair", egui::pos2(760.0, 600.0));
        let qr = centre(&h, Some(Role::Button), "QR code");
        f.hold(&mut h, 14);
        f.glide(&mut h, qr, 30);
        f.hold(&mut h, 4);
        f.click(&mut h);
        PIPELINE_STATS.lock().listen_addr = "192.0.2.10:9876".into();
        f.hold(&mut h, 20);
        note(&f, "dialog", h.get_by_label("Scan to connect").rect());
        f.glide(&mut h, egui::pos2(880.0, 610.0), 26);
        f.hold(&mut h, 50);
        connect_named(&shared, "iPad Pro", 10, 20);
        f.hold(&mut h, 14);
        if let Some(done) = h.query_by_label("Done") {
            let done = done.rect().center();
            f.glide(&mut h, done, 22);
            f.hold(&mut h, 3);
            f.click(&mut h);
        }
        let used = f.frame;
        f.hold(&mut h, 212 - used);
        f.glide(&mut h, egui::pos2(760.0, 600.0), 22);
        disconnect_ipad(&shared);
        f.hold(&mut h, 6);
        f.finish();
    }

    // Second screen: Mirror my screen, then Second screen, then back.
    if wants("second-screen") {
        let (_dir, shared) = fixture();
        connect_named(&shared, "iPad Pro", 10, 20);
        let mut h = film_harness(&shared, size);
        let mut f = Film::new(&root, "second-screen", egui::pos2(760.0, 420.0));
        let second = centre(&h, None, "Second screen");
        let mirror = centre(&h, None, "Mirror my screen");
        note(&f, "choice", h.get_by_label("Use the iPad as").rect());
        f.hold(&mut h, 16);
        f.glide(&mut h, second, 30);
        f.hold(&mut h, 4);
        f.click(&mut h);
        f.hold(&mut h, 10);
        *shared.vdd_status.lock() = VddStatus::Active;
        f.glide(&mut h, egui::pos2(720.0, 300.0), 26);
        f.hold(&mut h, 62);
        f.glide(&mut h, mirror, 30);
        f.hold(&mut h, 4);
        f.click(&mut h);
        *shared.vdd_status.lock() = VddStatus::Inactive;
        let used = f.frame;
        f.hold(&mut h, 212 - used);
        f.glide(&mut h, egui::pos2(760.0, 420.0), 28);
        f.finish();
    }

    // Performance: a live encode-time chart while the GPU behind it changes.
    if wants("performance") {
        let (_dir, shared) = fixture();
        connect_named(&shared, "iPad Pro", 10, 20);
        let gpus = [
            ("NVIDIA GeForce RTX 4070", "h264_nvenc"),
            ("AMD Radeon RX 7800 XT", "hevc_amf"),
            ("Intel Arc A750", "h264_qsv"),
        ];
        let feed = |frame: usize| {
            let mut stats = PIPELINE_STATS.lock();
            let (gpu, codec) = gpus[frame / 80];
            stats.gpu_name = gpu.into();
            stats.codec_name = codec.into();
            stats.encode_time_history = (0..300).map(|i| encode_sample(i + frame * 2)).collect();
            stats.encode_time_us = (encode_sample(frame * 2 + 299) * 1000.0) as u128;
        };
        feed(0);
        let mut h = film_harness(&shared, egui::vec2(1040.0, 1000.0));
        nav(&mut h, "Performance");
        let mut f = Film::new(&root, "performance", egui::pos2(-50.0, -50.0));
        note(&f, "gpu", h.get_by_label("GPU").rect());
        note(&f, "chart", h.get_by_label("Encode time").rect());
        for frame in 0..240 {
            feed(frame);
            f.shoot(&mut h, false);
        }
        f.finish();
    }

    // Feedback: open the panel from the sidebar, write a line, close it.
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
        let words = "Drawing on the iPad in Krita feels great.";
        for (i, ch) in words.chars().enumerate() {
            h.event(egui::Event::Text(ch.to_string()));
            f.shoot(&mut h, false);
            if i % 2 == 1 {
                f.shoot(&mut h, false);
            }
        }
        let send = centre(&h, Some(Role::Button), "Open in my email app");
        f.glide(&mut h, send, 18);
        f.hold(&mut h, 20);
        let close = centre(&h, Some(Role::Button), "Close");
        f.glide(&mut h, close, 16);
        f.hold(&mut h, 3);
        f.click(&mut h);
        let used = f.frame;
        assert!(used <= 214, "feedback runs long: {used}");
        f.glide(&mut h, egui::pos2(520.0, 520.0), 240 - used);
        f.finish();
    }
}
