//! onkey_core: OnKey's song-learning and pitch pipeline, built as an Android library.
//!
//! Reused from OnKey (https://github.com/dsmurrell/onkey, MIT, Copyright (c) 2026
//! Daniel Murrell). The algorithm modules are upstream code, lightly patched for
//! Android (Core ML and App Kit calls are cfg-gated; see engine.rs/capture.rs shims).
//! This file itself is new: the JNI entry points the Android app calls.
//!
//! MVP scope: learn a song from a file the user picked (UVR vocal separation + CREPE
//! pitch tracking) and track live mic pitch for the sing-along lane.

pub mod analysis;
pub mod beats;
pub mod bleed;
pub mod capture;
pub mod ctc;
pub mod devtest;
pub mod discard;
pub mod dsp;
pub mod engine;
pub mod game;
pub mod learn;
pub mod listen;
pub mod lowmem;
pub mod lyrics;
pub mod lyrics_align;
pub mod mdx;
pub mod mirror;
pub mod models;
pub mod music;
pub mod notes;
pub mod notmusic;
pub mod permissions;
pub mod pitch;
pub mod progress;
pub mod recognize;
pub mod records;
pub mod report;
pub mod resample;
pub mod settings;
pub mod source;
pub mod spotify;
pub mod store;
pub mod syllables;
pub mod take;
pub mod takeaudio;
pub mod timing;
pub mod tracker;
pub mod viz;
pub mod youtube;

use jni::objects::{JClass, JFloatArray, JString, JObject, JValue};
use jni::sys::{jfloat, jstring};
use jni::JNIEnv;

use learn::{Learner, Models, Separator, VocalPitch};

const KARA: &str = "UVR_MDXNET_KARA_2.onnx";
const CREPE_FULL: &str = "crepe-full-b32.onnx";

static CREPE_LIVE: std::sync::Mutex<Option<pitch::Crepe>> = std::sync::Mutex::new(None);

/// Call once at app start with the app's files dir: initialises settings storage.
#[no_mangle]
pub extern "system" fn Java_com_onkey_android_native_OnKeyCore_nativeInit(
    env: JNIEnv,
    _class: JClass,
    data_dir: JString,
) {
    let dir = match env.get_string(&data_dir) {
        Ok(s) => std::path::PathBuf::from(s.to_string_lossy().into_owned()),
        Err(_) => return,
    };
    let _ = std::fs::create_dir_all(&dir);
    settings::init(&dir);
}

#[no_mangle]
pub extern "system" fn Java_com_onkey_android_native_OnKeyCore_coreVersion(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    jstr(&env, "onkey-core 0.1.0 (OnKey 7a74cd9 pipeline)")
}

/// Learn a song: stereo interleaved f32 in, the singer's midi + periodicity per 10 ms
/// frame out (as JSON). Blocks for the length of the separation work; call off the UI
/// thread and pass a progress callback object with `void onProgress(float)`.
#[no_mangle]
pub extern "system" fn Java_com_onkey_android_native_OnKeyCore_nativeLearnSong(
    env: JNIEnv,
    _class: JClass,
    models_dir: JString,
    stereo: JFloatArray,
    rate: jfloat,
    progress: JObject,
) -> jstring {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        learn_song(&env, models_dir, stereo, rate, &progress)
    }));
    let json = match outcome {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => format!("{{\"error\":{}}}", serde_json::to_string(&e).unwrap_or_else(|_| "\"learn failed\"".into())),
        Err(_) => "{\"error\":\"panic in native learn\"}".into(),
    };
    jstr(&env, &json)
}

fn learn_song(
    env: &JNIEnv,
    models_dir: JString,
    stereo: JFloatArray,
    rate: jfloat,
    progress: &JObject,
) -> Result<String, String> {
    engine::touch();
    let dir = std::path::PathBuf::from(
        env.get_string(&models_dir).map_err(|e| e.to_string())?.to_string_lossy().into_owned(),
    );
    for f in [KARA, CREPE_FULL] {
        let p = dir.join(f);
        if !p.is_file() {
            return Err(format!("model missing: {}", p.display()));
        }
    }
    let len = env.get_array_length(&stereo).map_err(|e| e.to_string())? as usize;
    let mut buf = vec![0f32; len];
    env.get_float_array_region(&stereo, 0, &mut buf).map_err(|e| e.to_string())?;
    let rate = rate as f64;
    if len < 2 {
        return Err("no audio".into());
    }

    let kara = dir.join(KARA);
    let crepe = dir.join(CREPE_FULL);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(4);

    let load: Box<dyn FnOnce() -> Result<Models, String> + Send> = Box::new(move || {
        let sep = mdx::Mdx::load(&kara, threads, false)?;
        let p = pitch::Crepe::from_file(&crepe, threads, None).map_err(|e| format!("loading CREPE: {e}"))?;
        Ok((Box::new(sep) as Box<dyn Separator>, Box::new(p) as Box<dyn VocalPitch>))
    });

    let learner = Learner::start_with(load, rate);
    // push ~2 s chunks (stereo interleaved) so the caller sees progress move
    let chunk = (rate as usize * 2) * 2;
    let total = buf.len().max(1);
    for (i, c) in buf.chunks(chunk).enumerate() {
        engine::touch();
        learner.push(c.to_vec());
        let frac = ((i + 1) * chunk).min(total) as f32 / total as f32;
        report_progress(env, progress, frac);
    }
    let learnt = learner.finish()?;

    // Thin the 100 fps series to ~20 fps for the JSON payload (median of each 5-frame window).
    let midi = thin(&learnt.midi, 5);
    let per = thin(&learnt.periodicity, 5);
    let json = serde_json::json!({
        "fps": 20.0,
        "midi": midi,
        "periodicity": per,
    });
    Ok(json.to_string())
}

/// Live mic pitch: `x` is mono 16 kHz; returns midi (NaN when unvoiced).
#[no_mangle]
pub extern "system" fn Java_com_onkey_android_native_OnKeyCore_nativePitchFrame(
    env: JNIEnv,
    _class: JClass,
    x: JFloatArray,
) -> jfloat {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> f32 {
        let len = match env.get_array_length(&x) {
            Ok(l) => l as usize,
            Err(_) => return f32::NAN,
        };
        if len == 0 {
            return f32::NAN;
        }
        let mut buf = vec![0f32; len];
        if env.get_float_array_region(&x, 0, &mut buf).is_err() {
            return f32::NAN;
        }
        // (Re)create the tiny CREPE session lazily; it lives for the app's lifetime.
        let ok = CREPE_LIVE.lock().map(|mut g| {
            if g.is_none() {
                *g = pitch::Crepe::new().ok();
            }
            g.is_some()
        }).unwrap_or(false);
        if !ok {
            return f32::NAN;
        }
        let mut guard = CREPE_LIVE.lock().ok();
        let crepe = match guard.as_deref_mut() {
            Some(Some(c)) => c,
            _ => return f32::NAN,
        };
        match crepe.frames(&buf, 1) {
            Ok(frames) => frames.first().map(|f| f.midi).unwrap_or(f32::NAN),
            Err(_) => f32::NAN,
        }
    }));
    outcome.unwrap_or(f32::NAN)
}

fn report_progress(env: &JNIEnv, obj: &JObject, frac: f32) {
    let _ = env.call_method(obj, "onProgress", "(F)V", &[JValue::from(frac)]);
}

fn jstr(env: &JNIEnv, s: &str) -> jstring {
    match env.new_string(s) {
        Ok(j) => j.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Median of each `step`-frame window, NaN-tolerant.
fn thin(a: &[f32], step: usize) -> Vec<f32> {
    a.chunks(step)
        .map(|c| {
            let mut v: Vec<f32> = c.iter().cloned().filter(|x| !x.is_nan()).collect();
            if v.is_empty() {
                return f32::NAN;
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            v[v.len() / 2]
        })
        .collect()
}
