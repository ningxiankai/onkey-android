//! Platform shim replacing OnKey's macOS engine.rs for the Android core crate.
//! Upstream modules call `crate::engine::{log, write_wav, quitting, quiet_for}`;
//! on Android the app drives the pipeline from Kotlin, so these are thin, honest
//! implementations (log -> logcat via the `log` crate, no Tauri event system).
//!
//! Reused from OnKey (MIT, Copyright (c) 2026 Daniel Murrell, github.com/dsmurrell/onkey).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

static QUITTING: AtomicBool = AtomicBool::new(false);
static LAST_ACTIVITY_MS: AtomicU64 = AtomicU64::new(0);

pub fn log(msg: &str) {
    log::info!("onkey: {msg}");
}

pub fn quitting() -> bool {
    QUITTING.load(Ordering::Relaxed)
}

pub fn set_quitting(on: bool) {
    QUITTING.store(on, Ordering::Relaxed);
}

/// How long since the app last had anything to do (Kotlin touches this between jobs).
pub fn quiet_for() -> Duration {
    let last = LAST_ACTIVITY_MS.load(Ordering::Relaxed);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Duration::from_millis(now.saturating_sub(last))
}

pub fn touch() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    LAST_ACTIVITY_MS.store(now, Ordering::Relaxed);
}

/// 16-bit mono WAV (verbatim from upstream engine.rs).
pub fn write_wav(path: &std::path::Path, samples: &[i16], rate: u32) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    crate::store::write_atomic(path, &b)
}
