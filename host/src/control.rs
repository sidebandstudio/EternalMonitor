use crate::transport::link::PeerId;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc};

use parking_lot::Mutex;
use tracing::warn;

pub const DEFAULT_TARGET_FPS: u32 = 60;

#[derive(Clone)]
pub struct SharedControl {
    pub running: Arc<AtomicBool>,
    /// The user's MAXIMUM bitrate (the GUI slider) — the ABR ceiling.
    pub bitrate_bps: Arc<AtomicU32>,
    /// What the encoder should currently produce: the ABR controller's pick,
    /// always <= `bitrate_bps`. The encoder reopens its session when this
    /// changes for hardware encoders. libx264 reconfigures the open context.
    pub abr_current_bps: Arc<AtomicU32>,
    pub target_fps: Arc<AtomicU32>,
    pub client_preferred_fps: Arc<AtomicU32>,
    pub encoder_input: Arc<Mutex<crate::encoder::input::EncoderInput>>,
    pub max_dgram: Arc<AtomicU32>,
    pub target_addr: Arc<Mutex<PeerId>>,
    /// Set when the client needs a keyframe: a new session, a keyframe request
    /// or frames dropped on a USB link. The encoder clears it on the next
    /// frame and forces an IDR, reopening AMF sessions because AMF ignores
    /// per-frame keyframe requests (see `encoder::ignores_forced_keyframes`).
    pub force_next_idr: Arc<AtomicBool>,
    /// Set when the client asks to restart the stream: the encoder reopens its
    /// session on the next frame (then forces an IDR), as for a bitrate change.
    pub reopen_encoder: Arc<AtomicBool>,
    /// Encoder name override from the Settings tab, applied on next pipeline restart.
    /// `None` means use auto-detected encoder from GpuInfo.
    pub encoder_override: Arc<Mutex<Option<String>>>,
    /// Which display output the capture loop should duplicate, applied on next pipeline
    /// restart. `PrimaryAuto` mirrors the primary monitor (today's behavior).
    pub capture_target: Arc<Mutex<CaptureTarget>>,
    /// Live status of the managed virtual extended display, surfaced in the GUI so a tester
    /// can see when an Extended-display request silently fell back to the primary monitor.
    pub vdd_status: Arc<Mutex<VddStatus>>,
    /// Why the second screen did not appear, when Windows said, so the GUI
    /// can say what to do. Cleared at each attempt.
    pub vdd_problem: Arc<Mutex<Option<crate::vdd_device::VddProblem>>>,
    /// Set while Windows blocks screen capture (lock screen, UAC, a display
    /// it will not duplicate); the GUI explains it instead of a frozen picture.
    pub capture_block: Arc<Mutex<Option<crate::capture::recovery::CaptureBlock>>>,
    /// With `UnsupportedOnThisGpu`: the card the captured screen is wired to
    /// and the switch that would move EternalMonitor onto it.
    pub gpu_diagnosis: Arc<Mutex<Option<crate::gpu_preference::Diagnosis>>>,
    /// A switch made from that notice, as saved in settings. Capture
    /// records its outcome here; the GUI saves it.
    pub gpu_switch: Arc<Mutex<Option<crate::gpu_preference::SwitchRecord>>>,
    /// This process saved a graphics card change. Windows applies it only
    /// when EternalMonitor starts, so this process's own captures say
    /// nothing about whether it helped (a restart that didn't happen, or
    /// the moments before this process leaves). Set and read under
    /// `gpu_switch`'s lock (`capture::recovery::save_gpu_switch`).
    pub gpu_changed_here: Arc<std::sync::atomic::AtomicBool>,
    /// The graphics cards and screens, for Settings > Troubleshooting. The
    /// GUI reads them on Windows when the section opens.
    pub gpu_facts: Arc<Mutex<Option<crate::gpu_preference::GpuFacts>>>,
    /// An update or a restart handing over to another copy (one at a time).
    pub handoff: crate::handoff::Gate,
    /// The one thread that turns the virtual display on and off, and its
    /// queue.
    pub(crate) vdd_worker: Arc<crate::capture::second_screen::Worker>,
    /// Output owned by the enabled driver, retained across pipeline restarts.
    pub(crate) vdd_output: Arc<Mutex<Option<String>>>,
    /// Second screen timing across pipeline restarts: the pending switch onto
    /// it, the grace period after its viewer left, the last driver change.
    pub(crate) second_screen: Arc<Mutex<crate::capture::second_screen::State>>,
    /// Watchdog heartbeats (ms on the process clock, relaxed): the capture
    /// loop's aliveness, the last produced frame, and the last encoded frame.
    /// The supervisor detects wedged/starved stages from these.
    pub hb_capture_loop_ms: Arc<AtomicU64>,
    pub hb_capture_frame_ms: Arc<AtomicU64>,
    pub hb_encode_frame_ms: Arc<AtomicU64>,
    /// Desktop-space rectangle of the output currently being captured, set by
    /// the capture stage at (re)init. The input relay maps the client's
    /// normalized touch coordinates onto this rect. `None` until the first
    /// capture stage comes up.
    pub capture_geometry: Arc<Mutex<Option<crate::input::CaptureGeometry>>>,
    /// The user's "prefer HEVC" setting. The encoder switches to an H.265
    /// session only when this is on AND the connected client advertised HEVC
    /// decode in its HELLO2.
    pub hevc_enabled: Arc<AtomicBool>,
    /// Host permission to capture and stream PC audio. The client must also
    /// negotiate FEATURE_WANTS_AUDIO before the audio stage opens an endpoint.
    pub audio_enabled: Arc<AtomicBool>,
    /// What the encoder is actually emitting right now (wire `CODEC_*` value),
    /// written on every encoder open and reported in HELLO_ACK/heartbeats.
    pub active_codec: Arc<AtomicU8>,
    /// The client session. Lives here (not in the transport task) so a
    /// pipeline restart — crash recovery, virtual-display toggle, settings
    /// change — does NOT force the iPad through a fresh handshake: the new
    /// transport resumes streaming to the same session and the epoch bump
    /// resets the client's reassembly.
    pub session: Arc<Mutex<crate::transport::session::Session>>,
    pub pairing: Arc<Mutex<crate::pairing::Pairing>>,
}

/// Runtime state of the managed virtual extended display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VddStatus {
    /// Not using the managed virtual display (mirroring or a real output).
    Inactive,
    /// Virtual display requested but not yet active because no iPad has connected. The driver
    /// is deliberately left off until a receiver registers, so an idle PC shows no phantom monitor.
    WaitingForClient,
    /// The display is being turned on while the iPad gets the mirror.
    /// Reconnects and disconnects leave the start alone.
    Attaching,
    /// Virtual display enabled and being captured.
    Active,
    /// Virtual display was requested but did not attach — capture fell back to the primary
    /// display. Later connections mirror too, until the user tries again or changes the mode.
    Failed,
    /// The installer-owned task itself failed to execute. Remembered like `Failed`.
    TaskFailed,
}

/// Selects which display output the capture loop duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureTarget {
    /// Capture the primary desktop output (the one at (0,0)). Default.
    PrimaryAuto,
    /// Capture the output whose DXGI `DeviceName` matches this string (e.g. `\\.\DISPLAY3`).
    Output(String),
    /// Capture the managed virtual extended display. The capture loop enables the bundled
    /// virtual display driver on demand (so no phantom monitor exists when unused), waits
    /// for its output to appear, and disables it again when the target changes.
    VirtualExtended,
}

/// The `capture_display` settings value that means [`CaptureTarget::VirtualExtended`].
pub const CAPTURE_VIRTUAL_SENTINEL: &str = "virtual";

impl CaptureTarget {
    /// Map the persisted `capture_display` setting to a target (shared by the
    /// GUI and the startup preload so generation 0 already captures the right
    /// display).
    pub fn from_setting(setting: Option<&str>) -> Self {
        match setting {
            Some(s) if s == CAPTURE_VIRTUAL_SENTINEL => CaptureTarget::VirtualExtended,
            Some(name) if !name.is_empty() => CaptureTarget::Output(name.to_string()),
            _ => CaptureTarget::PrimaryAuto,
        }
    }
}

impl SharedControl {
    pub fn new(listen_port: u16, initial_bitrate_bps: u32) -> Self {
        Self {
            pairing: Arc::new(Mutex::new(
                crate::pairing::Pairing::new(
                    true,
                    crate::pairing::new_token().expect("OS entropy for pairing token"),
                )
                .expect("OS entropy for pairing code"),
            )),
            running: Arc::new(AtomicBool::new(false)),
            bitrate_bps: Arc::new(AtomicU32::new(initial_bitrate_bps)),
            abr_current_bps: Arc::new(AtomicU32::new(initial_bitrate_bps)),
            target_fps: Arc::new(AtomicU32::new(DEFAULT_TARGET_FPS)),
            client_preferred_fps: Arc::new(AtomicU32::new(0)),
            encoder_input: Arc::new(Mutex::new(Default::default())),
            max_dgram: Arc::new(AtomicU32::new(eternal_wire::v2::MAX_DGRAM_SIZE as u32)),
            target_addr: Arc::new(Mutex::new(PeerId::udp(SocketAddr::from((
                [0, 0, 0, 0],
                listen_port,
            ))))),
            force_next_idr: Arc::new(AtomicBool::new(false)),
            reopen_encoder: Arc::new(AtomicBool::new(false)),
            encoder_override: Arc::new(Mutex::new(None)),
            capture_target: Arc::new(Mutex::new(CaptureTarget::PrimaryAuto)),
            vdd_status: Arc::new(Mutex::new(VddStatus::Inactive)),
            vdd_problem: Arc::new(Mutex::new(None)),
            capture_block: Arc::new(Mutex::new(None)),
            gpu_diagnosis: Arc::new(Mutex::new(None)),
            gpu_switch: Arc::new(Mutex::new(None)),
            gpu_changed_here: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            gpu_facts: Arc::new(Mutex::new(None)),
            handoff: Default::default(),
            vdd_worker: Arc::new(Default::default()),
            vdd_output: Arc::new(Mutex::new(None)),
            second_screen: Arc::new(Mutex::new(Default::default())),
            hb_capture_loop_ms: Arc::new(AtomicU64::new(0)),
            hb_capture_frame_ms: Arc::new(AtomicU64::new(0)),
            hb_encode_frame_ms: Arc::new(AtomicU64::new(0)),
            capture_geometry: Arc::new(Mutex::new(None)),
            hevc_enabled: Arc::new(AtomicBool::new(false)),
            audio_enabled: Arc::new(AtomicBool::new(true)),
            active_codec: Arc::new(AtomicU8::new(eternal_wire::v2::control::CODEC_H264)),
            session: Arc::new(Mutex::new(crate::transport::session::Session::new({
                use std::hash::{BuildHasher, Hasher};
                std::collections::hash_map::RandomState::new()
                    .build_hasher()
                    .finish() as u32
            }))),
        }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    pub fn effective_fps(&self) -> u32 {
        merge_fps(
            self.target_fps.load(Ordering::SeqCst),
            self.client_preferred_fps.load(Ordering::SeqCst),
        )
    }

    /// Is a client actually watching? Protocol v2 makes the session the only
    /// truth: media needs a session id, so a target address on its own (a
    /// persisted `target_ip`, a stale value from a previous client) must never
    /// read as "someone is connected" — that would keep the capture loop at
    /// full rate with nobody watching and bring the virtual display up with no
    /// viewer.
    pub fn client_connected(&self) -> bool {
        self.session.lock().is_active()
    }

    /// The user asked to try again: a second screen that failed to start is
    /// tried once more at the next connection.
    pub(crate) fn retry_second_screen(&self) {
        crate::capture::second_screen::retry(self);
    }
}

pub fn merge_fps(host: u32, client: u32) -> u32 {
    if client == 0 {
        host.max(1)
    } else {
        host.max(1).min(client)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorCommand {
    /// Tear the current pipeline down and start a fresh generation.
    Restart,
    /// Stop streaming (pipeline down, GUI stays). `Start` resumes.
    Stop,
    /// Start a fresh pipeline from Stopped/Failed.
    Start,
    Shutdown,
}

#[derive(Clone)]
pub struct GuiControl {
    pub shared: SharedControl,
    pub supervisor_tx: mpsc::Sender<SupervisorCommand>,
}

impl GuiControl {
    pub fn request_restart(&self) {
        crate::vdd::recheck_tasks();
        self.shared.retry_second_screen();
        if let Err(error) = self.supervisor_tx.send(SupervisorCommand::Restart) {
            warn!(error = %error, "Failed to send restart command");
        }
    }

    pub fn request_stop(&self) {
        if let Err(error) = self.supervisor_tx.send(SupervisorCommand::Stop) {
            warn!(error = %error, "Failed to send stop command");
        }
    }

    pub fn request_start(&self) {
        crate::vdd::recheck_tasks();
        self.shared.retry_second_screen();
        if let Err(error) = self.supervisor_tx.send(SupervisorCommand::Start) {
            warn!(error = %error, "Failed to send start command");
        }
    }

    pub fn request_shutdown(&self) {
        if let Err(error) = self.supervisor_tx.send(SupervisorCommand::Shutdown) {
            warn!(error = %error, "Failed to send shutdown command");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_fps_caps_the_host_without_overwriting_its_preference() {
        let shared = SharedControl::new(9876, 15_000_000);
        for (host, client, expected) in [
            (120, 30, 30),
            (30, 120, 30),
            (90, 60, 60),
            (120, 0, 120),
            (0, 0, 1),
        ] {
            shared.target_fps.store(host, Ordering::SeqCst);
            shared.client_preferred_fps.store(client, Ordering::SeqCst);
            assert_eq!(shared.effective_fps(), expected);
            assert_eq!(shared.target_fps.load(Ordering::SeqCst), host);
        }
    }

    #[test]
    fn client_connected_requires_a_session_not_just_an_address() {
        let shared = SharedControl::new(9876, 15_000_000);
        assert!(!shared.client_connected(), "no session, no client");

        // A target address on its own — a persisted setting, or a leftover
        // from a client that has since gone — must not read as "connected".
        // It used to, which held the virtual display up and kept the capture
        // loop at full rate with nobody watching.
        *shared.target_addr.lock() = PeerId::udp("192.168.1.50:9876".parse().unwrap());
        assert!(!shared.client_connected());
    }
}
