//! The Troubleshooting settings: what a support reply asks a user to change,
//! saved with the other settings instead of environment variables. Every
//! default means today's behaviour. An environment variable that a developer
//! or CI sets still wins, then the saved setting, then Automatic.
//!
//! Test and simulation knobs (ETERNAL_SIMULATE_*, the diagnostics endpoint,
//! fault injection and the like) stay environment-only and are not here.

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::encoder::input::EncoderInput;
use crate::settings::SettingsFile;

/// How the EternalMonitor window is drawn. Read once, before the window's
/// graphics instance is created.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowDrawing {
    /// The renderer's own choice, with its crash avoidance.
    #[default]
    Automatic,
    Dx12,
    Vulkan,
    /// Microsoft's software renderer: slow, but needs no graphics driver.
    Warp,
}

impl WindowDrawing {
    fn name(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Dx12 => "dx12",
            Self::Vulkan => "vulkan",
            Self::Warp => "warp",
        }
    }
}

/// Where a value in effect came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The default: nothing set.
    Automatic,
    /// The saved setting.
    Saved,
    /// This environment variable, which the app's own setting can't change.
    Environment(&'static str),
}

/// A value in effect and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting<T> {
    pub value: T,
    pub source: Source,
}

impl<T> Setting<T> {
    /// The environment variable that fixes this value, if one does.
    pub fn env(&self) -> Option<&'static str> {
        match self.source {
            Source::Environment(name) => Some(name),
            _ => None,
        }
    }
}

impl<T: Clone> Setting<T> {
    /// The value to run with once the saved choice is `chosen`: a variable
    /// keeps its own value, whatever the setting says (Reset included).
    pub fn live(&self, chosen: T) -> T {
        match self.source {
            Source::Environment(_) => self.value.clone(),
            _ => chosen,
        }
    }
}

pub const ENCODER_ENV: &str = "ETERNAL_ENCODER";
pub const INPUT_ENV: &str = "ETERNAL_INPUT";
pub const HEVC_ENV: &str = "ETERNAL_HEVC";
pub const PACKET_ENV: &str = "ETERNAL_MAX_DGRAM";
pub const FPS_ENV: &str = "ETERNAL_FPS";
pub const WINDOW_ENV: &str = "WGPU_BACKEND";

/// The full-size packet; anything smaller is the VPN/hotspot setting.
pub const FULL_PACKET: u16 = 1400;
/// What "Smaller network packets" chooses: fits under VPN and hotspot
/// tunnels' extra headers.
pub const SMALL_PACKET: u16 = 1200;
const DEFAULT_FPS: u32 = 60;

/// The values in effect for each user-facing knob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overrides {
    /// The FFmpeg encoder name; `None` is Automatic.
    pub encoder: Setting<Option<String>>,
    pub encoder_input: Setting<EncoderInput>,
    pub hevc: Setting<bool>,
    pub max_dgram: Setting<u16>,
    pub target_fps: Setting<u32>,
    pub window: Setting<WindowDrawing>,
}

fn saved<T: PartialEq>(value: T, default: T) -> Setting<T> {
    let source = if value == default {
        Source::Automatic
    } else {
        Source::Saved
    };
    Setting { value, source }
}

impl Overrides {
    /// Resolves each knob from `env` (a variable's value, if set) and the
    /// saved settings. Fails only for an ETERNAL_INPUT the app can't use,
    /// which stops the app at start as before.
    pub fn resolve(
        saved_file: &SettingsFile,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let set = |name: &'static str| {
            env(name)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let encoder = match set(ENCODER_ENV) {
            Some(name) => Setting {
                value: Some(name),
                source: Source::Environment(ENCODER_ENV),
            },
            None => saved(saved_file.encoder_override.clone(), None),
        };
        let encoder_input = match set(INPUT_ENV) {
            Some(value) => Setting {
                value: EncoderInput::from_env(&value).ok_or_else(|| {
                    format!("Invalid ETERNAL_INPUT {value:?}; use auto, bgra or yuv420")
                })?,
                source: Source::Environment(INPUT_ENV),
            },
            None => saved(saved_file.encoder_input, EncoderInput::default()),
        };
        let hevc = match set(HEVC_ENV) {
            Some(value) => Setting {
                value: value == "1",
                source: Source::Environment(HEVC_ENV),
            },
            None => saved(saved_file.hevc_enabled, false),
        };
        let clamp = |bytes: u16| bytes.clamp(576, FULL_PACKET);
        let max_dgram = match set(PACKET_ENV).and_then(|value| value.parse::<u16>().ok()) {
            Some(bytes) => Setting {
                value: clamp(bytes),
                source: Source::Environment(PACKET_ENV),
            },
            None => saved(clamp(saved_file.max_dgram), FULL_PACKET),
        };
        let target_fps = match set(FPS_ENV)
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|&fps| fps > 0)
        {
            Some(fps) => Setting {
                value: fps,
                source: Source::Environment(FPS_ENV),
            },
            None if [30, 60, 90, 120].contains(&saved_file.target_fps) => {
                saved(saved_file.target_fps, DEFAULT_FPS)
            }
            None => saved(DEFAULT_FPS, DEFAULT_FPS),
        };
        let window = match set(WINDOW_ENV) {
            Some(_) => Setting {
                value: WindowDrawing::Automatic,
                source: Source::Environment(WINDOW_ENV),
            },
            None => saved(saved_file.window_drawing, WindowDrawing::Automatic),
        };
        Ok(Self {
            encoder,
            encoder_input,
            hevc,
            max_dgram,
            target_fps,
            window,
        })
    }

    /// From this process's environment.
    pub fn from_process(saved_file: &SettingsFile) -> Result<Self, String> {
        Self::resolve(saved_file, |name| std::env::var(name).ok())
    }

    /// The values that differ from Automatic, as `name=value (source)`,
    /// with no names of people, PCs or paths. Empty when all are Automatic.
    pub fn summary(&self, gpu_preference: Option<u32>) -> String {
        fn origin(source: Source) -> &'static str {
            match source {
                Source::Environment(_) => "environment",
                _ => "setting",
            }
        }
        let mut parts = Vec::new();
        let mut add = |name: &str, value: String, source: Source| {
            if source != Source::Automatic {
                parts.push(format!("{name}={value} ({})", origin(source)));
            }
        };
        add(
            "encoder",
            self.encoder.value.clone().unwrap_or_default(),
            self.encoder.source,
        );
        add(
            "encoder_input",
            format!("{:?}", self.encoder_input.value).to_lowercase(),
            self.encoder_input.source,
        );
        add(
            "hevc",
            if self.hevc.value { "on" } else { "off" }.into(),
            self.hevc.source,
        );
        add(
            "packet",
            self.max_dgram.value.to_string(),
            self.max_dgram.source,
        );
        add(
            "fps",
            self.target_fps.value.to_string(),
            self.target_fps.source,
        );
        add(
            "window",
            self.window.value.name().into(),
            self.window.source,
        );
        if let Some(preference) = gpu_preference.filter(|&value| value != 0) {
            parts.push(format!("graphics_card={preference} (windows)"));
        }
        parts.join(", ")
    }

    /// One line at start naming the non-default values, if any.
    pub fn log(&self, gpu_preference: Option<u32>) {
        let summary = self.summary(gpu_preference);
        if !summary.is_empty() {
            info!(overrides = %summary, "Troubleshooting settings in effect");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test]
    fn all_automatic_is_todays_behaviour_and_logs_nothing() {
        let overrides = Overrides::resolve(&SettingsFile::default(), env(&[])).unwrap();
        assert_eq!(overrides.encoder.value, None);
        assert_eq!(overrides.encoder_input.value, EncoderInput::Automatic);
        assert!(!overrides.hevc.value);
        assert_eq!(overrides.max_dgram.value, 1400);
        assert_eq!(overrides.target_fps.value, 60);
        assert_eq!(overrides.window.value, WindowDrawing::Automatic);
        for source in [
            overrides.encoder.source,
            overrides.encoder_input.source,
            overrides.hevc.source,
            overrides.max_dgram.source,
            overrides.target_fps.source,
            overrides.window.source,
        ] {
            assert_eq!(source, Source::Automatic);
        }
        assert_eq!(overrides.summary(None), "");
        assert_eq!(overrides.summary(Some(0)), "");
    }

    #[test]
    fn an_environment_variable_wins_over_the_saved_setting() {
        let saved_file = SettingsFile {
            encoder_override: Some("h264_qsv".into()),
            hevc_enabled: true,
            max_dgram: 1200,
            target_fps: 120,
            window_drawing: WindowDrawing::Vulkan,
            ..Default::default()
        };
        let overrides = Overrides::resolve(
            &saved_file,
            env(&[
                ("ETERNAL_ENCODER", " h264_amf "),
                ("ETERNAL_HEVC", "0"),
                ("ETERNAL_MAX_DGRAM", "9000"),
                ("ETERNAL_FPS", "30"),
                ("WGPU_BACKEND", "dx12"),
            ]),
        )
        .unwrap();
        assert_eq!(overrides.encoder.value.as_deref(), Some("h264_amf"));
        assert_eq!(overrides.encoder.env(), Some("ETERNAL_ENCODER"));
        // ETERNAL_HEVC=0 pins H.264 although the setting is on.
        assert!(!overrides.hevc.value);
        assert_eq!(overrides.hevc.env(), Some("ETERNAL_HEVC"));
        assert_eq!(overrides.max_dgram.value, 1400);
        assert_eq!(overrides.target_fps.value, 30);
        assert_eq!(overrides.window.env(), Some("WGPU_BACKEND"));
        // Without the variables, the saved settings apply.
        let overrides = Overrides::resolve(&saved_file, env(&[])).unwrap();
        assert_eq!(overrides.encoder.value.as_deref(), Some("h264_qsv"));
        assert_eq!(overrides.encoder.source, Source::Saved);
        assert!(overrides.hevc.value);
        assert_eq!(overrides.max_dgram.value, 1200);
        assert_eq!(overrides.target_fps.value, 120);
        assert_eq!(overrides.window.value, WindowDrawing::Vulkan);
        assert_eq!(
            overrides.summary(Some(1)),
            "encoder=h264_qsv (setting), hevc=on (setting), packet=1200 (setting), fps=120 (setting), window=vulkan (setting), graphics_card=1 (windows)"
        );
    }

    #[test]
    fn a_variable_keeps_its_value_when_the_setting_changes() {
        let saved_file = SettingsFile {
            encoder_override: Some("h264_qsv".into()),
            encoder_input: EncoderInput::Bgra,
            hevc_enabled: true,
            max_dgram: 1200,
            target_fps: 120,
            window_drawing: WindowDrawing::Vulkan,
            ..Default::default()
        };
        let fixed = Overrides::resolve(
            &saved_file,
            env(&[
                ("ETERNAL_ENCODER", "h264_amf"),
                ("ETERNAL_INPUT", "auto"),
                ("ETERNAL_HEVC", "1"),
                ("ETERNAL_MAX_DGRAM", "1000"),
                ("ETERNAL_FPS", "30"),
                ("WGPU_BACKEND", "dx12"),
            ]),
        )
        .unwrap();
        // Each knob back to Automatic, as Reset does: the variables' values
        // stay in effect.
        assert_eq!(fixed.encoder.live(None).as_deref(), Some("h264_amf"));
        assert_eq!(
            fixed.encoder_input.live(EncoderInput::default()),
            EncoderInput::Auto
        );
        assert!(fixed.hevc.live(false));
        assert_eq!(fixed.max_dgram.live(FULL_PACKET), 1000);
        assert_eq!(fixed.target_fps.live(DEFAULT_FPS), 30);
        assert_eq!(
            fixed.window.live(WindowDrawing::Automatic),
            WindowDrawing::Automatic
        );
        // Without variables, the new choice is what runs.
        let free = Overrides::resolve(&saved_file, env(&[])).unwrap();
        assert_eq!(free.encoder.live(None), None);
        assert_eq!(
            free.encoder_input.live(EncoderInput::default()),
            EncoderInput::default()
        );
        assert!(!free.hevc.live(false));
        assert_eq!(free.max_dgram.live(FULL_PACKET), FULL_PACKET);
        assert_eq!(free.target_fps.live(DEFAULT_FPS), DEFAULT_FPS);
        assert_eq!(
            free.window.live(WindowDrawing::Automatic),
            WindowDrawing::Automatic
        );
    }

    #[test]
    fn empty_or_unusable_variables_fall_back_to_the_setting() {
        let saved_file = SettingsFile {
            max_dgram: 1200,
            ..Default::default()
        };
        let overrides = Overrides::resolve(
            &saved_file,
            env(&[
                ("ETERNAL_ENCODER", "  "),
                ("ETERNAL_MAX_DGRAM", "big"),
                ("ETERNAL_FPS", "0"),
                ("WGPU_BACKEND", ""),
            ]),
        )
        .unwrap();
        assert_eq!(overrides.encoder.source, Source::Automatic);
        assert_eq!(overrides.max_dgram.value, 1200);
        assert_eq!(overrides.target_fps.source, Source::Automatic);
        assert_eq!(overrides.window.source, Source::Automatic);
        // An encoder input the app can't use stops the start, as before.
        assert!(Overrides::resolve(&saved_file, env(&[("ETERNAL_INPUT", "rgb")])).is_err());
    }

    #[test]
    fn the_start_line_names_no_person_pc_or_path() {
        let overrides = Overrides::resolve(
            &SettingsFile {
                encoder_override: Some("libx264".into()),
                ..Default::default()
            },
            env(&[("ETERNAL_INPUT", "bgra")]),
        )
        .unwrap();
        let summary = overrides.summary(Some(2));
        assert_eq!(
            summary,
            "encoder=libx264 (setting), encoder_input=bgra (environment), graphics_card=2 (windows)"
        );
        for leak in ['\\', '/', ':'] {
            assert!(!summary.contains(leak), "{summary}");
        }
    }
}
