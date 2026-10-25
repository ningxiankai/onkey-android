//! Android stub for OnKey's macOS capture.rs. The upstream module wraps Core Audio
//! (system-audio tap, mic enumeration, watchdogs). This MVP learns from files the user
//! picks, so only the shape of the API is kept; the system-facing parts report honest
//! defaults. Upstream API: OnKey (MIT, Copyright (c) 2026 Daniel Murrell).

use serde_json::json;
use serde_json::Value;

pub const CHANNELS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CaptureError {
    /// The music app isn't running or hasn't played anything yet.
    AppSilent,
    /// The system refused the audio tap (permission).
    TapRefused,
    /// Couldn't set up or start the audio device.
    DeviceFailed,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::AppSilent => write!(f, "the music app isn't playing"),
            CaptureError::TapRefused => write!(f, "audio capture was refused"),
            CaptureError::DeviceFailed => write!(f, "couldn't start the audio device"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Mic {
    pub uid: String,
    pub name: String,
}

pub struct Capture;

impl Capture {
    /// Upstream taps another app's audio via Core Audio. Android MVP learns from files
    /// instead; live capture lands with AudioPlaybackCapture in a later phase.
    pub fn start(_bundle: &str) -> Result<Capture, CaptureError> {
        Err(CaptureError::DeviceFailed)
    }

    pub fn drain_into(&mut self, _out: &mut Vec<f32>) {}
}

pub fn chosen_mic() -> Option<Mic> {
    Some(Mic { uid: "android-mic".into(), name: "Android microphone".into() })
}

pub fn mics() -> Vec<Mic> {
    vec![chosen_mic().unwrap()]
}

pub fn set_mic_preference(_uid: &str) {}

pub fn output_uid() -> String {
    "android-output".into()
}

pub fn output_name() -> String {
    "Android output".into()
}

pub fn output_latency_ms() -> i32 {
    40
}

pub fn app_sounding(_bundle: &str) -> bool {
    false
}

pub fn webkit_separable() -> bool {
    false
}

pub fn wake_count() -> i32 {
    0
}

pub fn elsewhere(_bundle: &str) -> bool {
    false
}

pub fn any_sound(block: &[f32]) -> bool {
    block.iter().any(|&f| f.abs() > 0.0001)
}

/// Rough RMS level meter, same shape as upstream.
pub struct MusicLevel {
    level_db: f32,
}

impl MusicLevel {
    pub const QUIET_DB: f32 = -55.0;

    pub fn new() -> Self {
        MusicLevel { level_db: f32::NEG_INFINITY }
    }

    pub fn measure(&mut self, block: &[f32], _rate: f64) {
        if block.is_empty() {
            return;
        }
        let sum: f32 = block.iter().map(|f| f * f).sum();
        let rms = (sum / block.len() as f32).sqrt();
        self.level_db = 20.0 * rms.max(1e-9).log10();
    }

    pub fn level_db(&self) -> f32 {
        self.level_db
    }

    pub fn quiet(&self) -> bool {
        self.level_db < Self::QUIET_DB
    }
}

impl Default for MusicLevel {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Watchdog {
    started: bool,
}

impl Watchdog {
    pub const QUIET_S: u64 = 5;
    pub const DEAD_S: u64 = 10;

    pub fn started(&mut self) {
        self.started = true;
    }

    pub fn check(&mut self, _anything: bool, _playing: bool, _bundle: &str) -> Option<&'static str> {
        let _ = self.started;
        None
    }

    pub fn elsewhere(&self) -> bool {
        false
    }
}

impl Default for Watchdog {
    fn default() -> Self {
        Watchdog { started: false }
    }
}

/// Permissions shape used elsewhere (nothing to ask for on Android yet).
pub fn status() -> Value {
    json!({})
}
