//! The full score after a take, port of the Python engine's analyze(): your whole recording is pitch-tracked
//! again (with smoothing the live view can't afford), your latency is found from the melody's shape, and you
//! get a score, notes hit, and a result per lyric line. A first play also yields the song's learned line.
use serde_json::{json, Value};

use crate::dsp::{self, credit, fold, shape_hits, tolerant_error, FP_BANDS, FRAME_S, TOL};
use crate::notes::NoteTracker;
use crate::source::Track;

pub enum Reference {
    /// a learned song: the singer's line in take frames (NaN = silent or unknown)
    Learned(Vec<f32>),
    /// a first play: the separated vocal (16 kHz) and its pitch (midi, periodicity) per frame
    Separated { vocal16: Vec<f32>, midi: Vec<f32>, periodicity: Vec<f32> },
}

pub struct Input {
    pub track: Track,
    pub recorded: String,
    pub mode: &'static str,
    pub blind: bool,
    pub lyrics: Vec<(f64, String)>,
    pub song_offset: f64,
    /// a first play's stretches between jumps, each at its own place (Take::stretches: [from, to) take frames,
    /// song offset in frames), for what's learned; empty: `song_offset` throughout
    pub places: Vec<(i64, i64, i64)>,
    pub reference: Reference,
    /// your pitch for the whole take: (midi, periodicity, rms) from pitch::track
    pub usr: (Vec<f32>, Vec<f32>),
    pub mic16: Vec<f32>,
    /// when you were singing, as judged live (on the echo-cancelled microphone); None: judged here from
    /// mic16's loudness (the Python prototype's rule, for comparisons)
    pub voiced: Option<Vec<bool>>,
    pub live: Value,
    /// first play: the take's rhythm fingerprint and which frames had Spotify audio
    pub fingerprint: Option<Vec<[f32; FP_BANDS]>>,
    pub audible: Option<Vec<bool>>,
    /// the notes to score (take frames), e.g. syllable bars; None: from the singer's line
    pub bars: Option<Vec<crate::notes::Bar>>,
}

pub struct Output {
    pub result: Value,
    /// first play: (song line, covered, fingerprint) in song frames, to merge into the learned song
    pub learned: Option<(Vec<f32>, Vec<bool>, Vec<[f32; FP_BANDS]>)>,
}

/// Loudness per 10 ms frame (dB), over the 20 ms around the frame.
fn frame_rms_db(x: &[f32], n: usize) -> Vec<f32> {
    (0..n).map(|i| dsp::frame_db(x, i)).collect()
}

pub fn analyze(inp: Input) -> Output {
    let (ref_m, ref_v, from_cache): (Vec<f32>, Vec<bool>, bool) = match &inp.reference {
        Reference::Learned(line) => (line.iter().map(|v| if v.is_nan() { 0.0 } else { *v }).collect(),
                                     line.iter().map(|v| !v.is_nan()).collect(), true),
        Reference::Separated { midi, periodicity, vocal16 } => {
            let db = frame_rms_db(vocal16, midi.len());
            let top = db.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            (midi.clone(), periodicity.iter().zip(&db).map(|(p, d)| *p > 0.45 && *d > top - 35.0).collect(), false)
        }
    };
    let n = ref_m.len().min(inp.usr.0.len());
    let ref_m = &ref_m[..n];
    let ref_v = &ref_v[..n];
    let mut usr_m = inp.usr.0[..n].to_vec();
    let usr_p = &inp.usr.1[..n];
    let usr_db = frame_rms_db(&inp.mic16, n);
    let gate = dsp::singing_gate(&usr_db);
    // singing: confident pitch, and (as live) sound above the background; the live judgement is on the
    // echo-cancelled microphone, so it holds up where music from speakers or a noisy room raises the
    // background (the loudness rule here then threw away real singing: final scores far under live ones)
    let mut usr_v: Vec<bool> = match &inp.voiced {
        Some(v) => (0..n).map(|i| usr_p[i] > 0.45 && (i.saturating_sub(2)..(i + 3).min(v.len())).any(|j| v[j])).collect(),
        None => (0..n).map(|i| usr_p[i] > 0.45 && usr_db[i] > gate).collect(),
    };

    // latency: shift your track so it best lines up with the singer (you hear the song late)
    let (mut best, mut best_lag) = (-1i64, 0i64);
    for k in -30i64..=150 {
        let (a0, b0, len) = if k >= 0 { (0, k as usize, n.saturating_sub(k as usize)) } else { ((-k) as usize, 0, n.saturating_sub((-k) as usize)) };
        let (mut u, mut r) = (Vec::new(), Vec::new());
        for j in 0..len {
            if ref_v[a0 + j] && usr_v[b0 + j] {
                u.push(usr_m[b0 + j]);
                r.push(ref_m[a0 + j]);
            }
        }
        let hits = shape_hits(&u, &r, 100.0) as i64;
        if hits > best {
            best = hits;
            best_lag = k;
        }
    }
    if best < 100 {
        best_lag = 20;   // under a second of matching singing: the estimate is noise, assume typical
    }
    roll(&mut usr_m, best_lag);
    roll(&mut usr_v, best_lag);
    if best_lag > 0 {
        usr_v[n - best_lag as usize..].iter_mut().for_each(|v| *v = false);
    } else if best_lag < 0 {
        usr_v[..(-best_lag) as usize].iter_mut().for_each(|v| *v = false);
    }

    let mut ext = vec![f32::NAN; TOL];
    ext.extend((0..n).map(|i| if ref_v[i] { ref_m[i] } else { f32::NAN }));
    ext.extend(vec![f32::NAN; TOL]);
    let usr_line: Vec<f32> = (0..n).map(|i| if usr_v[i] { usr_m[i] } else { f32::NAN }).collect();
    // each moment scores against the closer of the singer's line and the note bar over it
    let bars_for_err = inp.bars.clone().unwrap_or_else(|| NoteTracker::bars(&(0..n).map(|i| if ref_v[i] { ref_m[i] } else { f32::NAN }).collect::<Vec<_>>()));
    let mut bar_ext = vec![f32::NAN; n + 2 * TOL];
    for &(s, e, m) in &bars_for_err {
        for f in s.max(0)..e.min(n as i64) {
            bar_ext[f as usize + TOL] = m as f32;
        }
    }
    let err_line = tolerant_error(&ext, &usr_line, TOL, 50);
    let err_bar = tolerant_error(&bar_ext, &usr_line, TOL, 50);
    let err: Vec<f32> = err_line.iter().zip(&err_bar)
        .map(|(l, b)| if !b.is_nan() && (l.is_nan() || b.abs() < l.abs()) { *b } else { *l })
        .map(|e| if e.is_nan() { 0.0 } else { e }).collect();
    let both: Vec<bool> = (0..n).map(|i| ref_v[i] && usr_v[i]).collect();
    let nb = both.iter().filter(|b| **b).count();
    let nr = ref_v.iter().filter(|b| **b).count();
    let mean_over = |f: &dyn Fn(usize) -> f32| -> f32 {
        if nb == 0 { 0.0 } else { (0..n).filter(|&i| both[i]).map(|i| f(i) as f64).sum::<f64>() as f32 / nb as f32 }
    };
    let accuracy = mean_over(&|i| credit(err[i]));
    let coverage = nb as f32 / nr.max(1) as f32;
    let score = dsp::score(accuracy, coverage);
    let mut eb: Vec<f32> = (0..n).filter(|&i| both[i]).map(|i| err[i]).collect();
    let bias = if nb > 0 { dsp::median(&mut eb) } else { 0.0 };
    let within50 = mean_over(&|i| if err[i].abs() <= 50.0 { 1.0 } else { 0.0 });

    // per lyric line, in recording time
    let mut lines = Vec::new();
    let off = inp.song_offset;
    for (i, (t, text)) in inp.lyrics.iter().enumerate() {
        let t0 = t - off;
        let t1 = inp.lyrics.get(i + 1).map_or(n as f64 * FRAME_S, |l| l.0 - off);
        let f0 = ((t0 / FRAME_S) as i64).max(0) as usize;
        let f1 = ((t1 / FRAME_S) as i64).clamp(0, n as i64) as usize;
        if f1 <= f0 || !text.chars().any(|c| c.is_alphanumeric()) {
            continue;
        }
        let rv = ref_v[f0..f1].iter().filter(|b| **b).count();
        let idx: Vec<usize> = (f0..f1).filter(|&k| both[k]).collect();
        let mut e: Vec<f32> = idx.iter().map(|&k| err[k]).collect();
        let acc = (!idx.is_empty()).then(|| idx.iter().map(|&k| credit(err[k])).sum::<f32>() / idx.len() as f32);
        lines.push(json!({"t0": t0, "t1": t1, "text": text, "ref_frames": rv,
            "coverage": idx.len() as f32 / rv.max(1) as f32, "accuracy": acc,
            "bias": (!e.is_empty()).then(|| dsp::median(&mut e))}));
    }

    // note by note: a note is hit when you hold it within a semitone for at least half its length (its first
    // 80 ms, where singers scoop up, don't count), forgiving up to 80 ms early/late
    let bars = inp.bars.clone().unwrap_or_else(|| NoteTracker::bars(&(0..n).map(|i| if ref_v[i] { ref_m[i] } else { f32::NAN }).collect::<Vec<_>>()));
    let mut notes = Vec::new();
    for (a, b, m) in bars {
        if b - a < 15 {
            continue;
        }
        let mut held = 0f32;
        for sh in (-8i64..=8).step_by(2) {
            let (a2, b2) = (a + 8 + sh, b + sh);
            if a2 < 0 || b2 > n as i64 {
                continue;
            }
            let ok = (a2..b2).filter(|&k| usr_v[k as usize] && fold(100.0 * (usr_m[k as usize] - m as f32)).abs() <= 100.0).count();
            held = held.max(ok as f32 / (b2 - a2) as f32);
        }
        notes.push(json!([a, b, m, (held * 100.0).round() / 100.0]));
    }
    let notes_hit = notes.iter().filter(|nt| nt[3].as_f64().unwrap_or(0.0) >= 0.5).count();

    let r2 = |v: f32| (v * 100.0).round() / 100.0;
    let result = json!({
        "track": inp.track, "recorded": inp.recorded, "mode": inp.mode, "blind": inp.blind,
        "notes": notes, "notes_hit": notes_hit, "notes_total": notes.len(),
        "score": score.round() as i64, "accuracy": accuracy, "coverage": coverage,
        "within50": within50, "bias_cents": bias, "latency_ms": best_lag * 10,
        "frame_s": FRAME_S, "lines": lines,
        "ref": (0..n).map(|i| ref_v[i].then(|| r2(ref_m[i]))).collect::<Vec<_>>(),
        // your contour moved by whole octaves to the note bar over it (else the singer's line), as the live lane
        // shows it: the singer's line can sit an octave off the bars, which put you an octave away from them
        "usr": (0..n).map(|i| usr_v[i].then(|| {
            let b = bar_ext[i + TOL];
            let base = if b.is_nan() { ref_m[i] } else { b };
            r2(if base.is_nan() { usr_m[i] } else { base + fold(100.0 * (usr_m[i] - base)) / 100.0 })
        })).collect::<Vec<_>>(),
        "err": (0..n).map(|i| both[i].then(|| err[i].round())).collect::<Vec<_>>(),
        "live": inp.live,
    });

    // remember the singer's line in song time, so the next sing of this track is scored live from it
    let learned = (!from_cache).then(|| {
        let k0 = (off / FRAME_S).round() as i64;
        let place = |i: usize| inp.places.iter().find(|p| (p.0..p.1).contains(&(i as i64))).map_or(k0, |p| p.2);
        let total = (inp.track.duration / FRAME_S) as usize + 1;
        let mut song = vec![f32::NAN; total];
        let mut covered = vec![false; total];
        let mut fp = vec![[0f32; FP_BANDS]; total];
        for i in 0..n {
            let j = i as i64 + place(i);
            if j >= 0 && (j as usize) < total {
                song[j as usize] = if ref_v[i] { ref_m[i] } else { f32::NAN };
                // learned only where Spotify's audio was actually heard
                covered[j as usize] = inp.audible.as_ref().map_or(true, |a| a.get(i).copied().unwrap_or(false));
            }
        }
        if let Some(f) = &inp.fingerprint {
            for (i, row) in f.iter().enumerate() {
                let j = i as i64 + place(i);
                if j >= 0 && (j as usize) < total {
                    fp[j as usize] = *row;
                }
            }
        }
        (song, covered, if inp.fingerprint.is_some() { fp } else { Vec::new() })
    });
    Output { result, learned }
}

/// numpy.roll(x, -k)
fn roll<T: Clone>(x: &mut [T], k: i64) {
    let n = x.len() as i64;
    if n == 0 {
        return;
    }
    let k = k.rem_euclid(n) as usize;
    x.rotate_left(k);
}
