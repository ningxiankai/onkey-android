//! Scoring maths and the rhythm fingerprint, ported line for line from the Python engine (live.py).
//! Pitch is in MIDI note numbers per 10 ms frame, NaN where there is no voice.
use realfft::RealFftPlanner;

use crate::pitch::HOP;

pub const FRAME_S: f64 = 0.01;
/// frames: a note sung up to 80 ms early/late still counts as on the singer's pitch
pub const TOL: usize = 8;
pub const FP_BANDS: usize = 8;

/// Signed cents error with octave errors forgiven, in [-600, 600).
pub fn fold(c: f32) -> f32 {
    (c + 600.0).rem_euclid(1200.0) - 600.0
}

/// Score credit for a cents error: full within 50 cents, none beyond 150.
pub fn credit(err: f32) -> f32 {
    ((150.0 - err.abs()) / 100.0).clamp(0.0, 1.0)
}

/// Signed cents error of `usr` vs the singer, forgiving timing but not pitch.
/// Each half-second window gets the one shift (within ±tol frames) that lines you up best with the singer,
/// and every frame in that window is measured at that shift. `ref_ext` covers the usr frames plus tol frames
/// either side.
pub fn tolerant_error(ref_ext: &[f32], usr: &[f32], tol: usize, win: usize) -> Vec<f32> {
    let n = usr.len();
    let shifts = 2 * tol + 1;
    let e = |k: usize, i: usize| fold(100.0 * (usr[i] - ref_ext[k + i]));
    let mut out = vec![f32::NAN; n];
    let mut w0 = 0;
    while w0 < n {
        let w1 = (w0 + win).min(n);
        let mut best = (f64::INFINITY, tol);
        for k in 0..shifts {
            let mut cost = 0.0f64;
            for i in w0..w1 {
                let v = e(k, i);
                cost += if v.is_nan() { 300.0 } else { v.abs().min(300.0) as f64 };
            }
            let c = cost / (w1 - w0) as f64 - if k == tol { 1e-6 } else { 0.0 };   // ties go to no shift
            if c < best.0 {
                best = (c, k);
            }
        }
        for i in w0..w1 {
            let v = e(best.1, i);
            // where the shifted singer line has a gap, measure against the unshifted frame
            out[i] = if v.is_nan() { e(tol, i) } else { v };
        }
        w0 = w1;
    }
    out
}

/// Frames that line up once your overall offset from the singer is removed (melody shape, for latency).
pub fn shape_hits(usr: &[f32], reference: &[f32], tol: f32) -> usize {
    let mut e: Vec<f32> = usr.iter().zip(reference).map(|(u, r)| fold(100.0 * (u - r))).collect();
    if e.is_empty() {
        return 0;
    }
    let med = median(&mut e.clone());
    e.iter_mut().filter(|v| fold(**v - med).abs() < tol).count()
}

pub fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return f32::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { 0.5 * (v[n / 2 - 1] + v[n / 2]) }
}

/// numpy-style percentile (linear interpolation)
pub fn percentile(v: &[f32], q: f32) -> f32 {
    let mut s: Vec<f32> = v.iter().copied().filter(|x| !x.is_nan()).collect();
    if s.is_empty() {
        return f32::NAN;
    }
    s.sort_by(|a, b| a.total_cmp(b));
    let pos = q / 100.0 * (s.len() - 1) as f32;
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(s.len() - 1);
    s[lo] + (s[hi] - s[lo]) * (pos - lo as f32)
}

/// Loudness of 10 ms frame `i` (dB), over the 20 ms around it (outside `x`: silence).
pub fn frame_db(x: &[f32], i: usize) -> f32 {
    let (a, b) = (i as i64 * HOP as i64 - HOP as i64, i as i64 * HOP as i64 + HOP as i64);
    let s: f64 = (a.max(0)..b.min(x.len() as i64)).map(|k| (x[k as usize] as f64).powi(2)).sum();
    20.0 * (((s / (2 * HOP) as f64).sqrt() + 1e-9).log10()) as f32
}

/// Frames this quiet (dB) are digital silence: the time a skip or a gap filled in, or past the recording's end.
pub const SILENT_DB: f32 = -150.0;

/// The singing check's bar (dB, raw mic loudness per frame): 10 dB over the take's quietest fifth and within 40 dB
/// of its loudest. Over speakers this is what tells you from the song leaking into the mic (the leak is about as
/// loud all through; you're louder). Digital silence isn't the room's quiet and is left out: counted, a skip over
/// a fifth of the take dropped the bar to 40 dB under the loudest, and the leak counted as singing again (a take
/// with nobody singing and a skip over its intro scored 40).
pub fn singing_gate(db: &[f32]) -> f32 {
    let heard: Vec<f32> = db.iter().copied().filter(|d| *d > SILENT_DB).collect();
    if heard.is_empty() {
        return f32::INFINITY;
    }
    let noise = percentile(&heard, 20.0);
    let top = heard.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    (noise + 10.0).max(top - 40.0)
}

/// A take's score (0-100) from how in tune you were (`accuracy`, 0-1) and how much of the singer's part you sang
/// (`coverage`, 0-1). Singing 40% of the part earns full credit: some of the part is backing vocals, ad-libs or
/// bars that aren't really there, and the singing check is strict over speakers, so asking for more marked people
/// down for parts they couldn't sing. Singing very little still scores little (a tenth of the part for any credit
/// at all to count fully), so one good line isn't a good take.
pub fn score(accuracy: f32, coverage: f32) -> f32 {
    100.0 * accuracy * (0.3 * (coverage / 0.1).min(1.0) + 0.7 * (coverage / 0.4).min(1.0))
}

/// Rhythm fingerprint, (frames × 8) at 10 ms: how much the sound just changed in each of 8 frequency bands
/// (positive spectral flux, slow trend removed, normalised). Enough to place a recording in the song; not audio.
pub fn onset_envelope(x16: &[f32]) -> Vec<[f32; FP_BANDS]> {
    if x16.len() < 2048 {
        return Vec::new();
    }
    let n = 1 + x16.len() / 160;
    let mut padded = vec![0f32; 512];
    padded.extend_from_slice(x16);
    padded.extend(std::iter::repeat(0.0).take(512));
    let hann: Vec<f32> = (0..1024).map(|k| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * k as f32 / 1023.0).cos()).collect();
    const EDGES: [usize; 9] = [4, 7, 13, 23, 42, 76, 137, 248, 448];   // ~60 Hz to 7 kHz at 16 kHz
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(1024);
    let mut inbuf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut loud = vec![[0f32; FP_BANDS]; n];
    for (f, row) in loud.iter_mut().enumerate() {
        let a = f * 160;
        if a + 1024 > padded.len() {
            break;
        }
        for i in 0..1024 {
            inbuf[i] = padded[a + i] * hann[i];
        }
        fft.process(&mut inbuf, &mut spec).unwrap();
        for b in 0..FP_BANDS {
            let s: f32 = spec[EDGES[b]..EDGES[b + 1]].iter().map(|c| c.norm()).sum();
            row[b] = (1000.0 * s).ln_1p();
        }
    }
    let mut env = vec![[0f32; FP_BANDS]; n];
    for b in 0..FP_BANDS {
        let flux: Vec<f32> = (0..n).map(|f| if f == 0 { 0.0 } else { (loud[f][b] - loud[f - 1][b]).max(0.0) }).collect();
        // slow trend: mean over the 100 frames [f-50, f+49] (numpy convolve "same" with a box of 100)
        let mut csum = vec![0f64; n + 1];
        for f in 0..n {
            csum[f + 1] = csum[f] + flux[f] as f64;
        }
        for f in 0..n {
            let lo = f.saturating_sub(50);
            let hi = (f + 50).min(n);
            env[f][b] = flux[f] - ((csum[hi] - csum[lo]) / 100.0) as f32;
        }
        let mean = env.iter().map(|r| r[b] as f64).sum::<f64>() / n as f64;
        let std = (env.iter().map(|r| (r[b] as f64 - mean).powi(2)).sum::<f64>() / n as f64).sqrt() as f32;
        for r in env.iter_mut() {
            r[b] /= std + 1e-6;
        }
    }
    env
}

/// The fewest fingerprint frames `locate` works from (3 s): it gives (0, 0.0) for less.
pub const LOCATE_MIN_FRAMES: usize = 300;

/// The fewest 16 kHz samples whose `onset_envelope` has LOCATE_MIN_FRAMES frames (1 + samples / 160 of them):
/// a placement tried on less audio is wasted.
pub const LOCATE_MIN_SAMPLES: usize = (LOCATE_MIN_FRAMES - 1) * 160;

/// Where in the song (frame) a stretch of live audio starts, searching starts in [lo, hi). Returns
/// (frame, confidence): the peak correlation over the next best one more than 0.3 s away.
pub fn locate(live: &[[f32; FP_BANDS]], song: &[[f32; FP_BANDS]], lo: i64, hi: Option<i64>) -> (i64, f32) {
    let n = live.len() as i64;
    if n < LOCATE_MIN_FRAMES as i64 || (song.len() as i64) < n {
        return (0, 0.0);
    }
    let max_hi = song.len() as i64 - n + 1;
    let hi = hi.map_or(max_hi, |h| h.min(max_hi));
    let lo = lo.max(0);
    if hi <= lo {
        return (0, 0.0);
    }
    let c: Vec<f32> = (lo..hi).map(|k| {
        let mut acc = 0f32;
        for (t, l) in live.iter().enumerate() {
            let s = &song[(k as usize) + t];
            for b in 0..FP_BANDS {
                acc += s[b] * l[b];
            }
        }
        acc
    }).collect();
    let (i, &peak) = c.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
    if peak <= 0.0 {
        return (lo + i as i64, 0.0);
    }
    let second = c.iter().enumerate()
        .filter(|(j, _)| (*j as i64 - i as i64).abs() > 30)
        .map(|(_, v)| *v)
        .fold(f32::NEG_INFINITY, f32::max);
    let second = if second.is_finite() { second } else { 0.0 };
    (lo + i as i64, if second > 0.0 { peak / second } else { 9.0 })
}

/// The best few places (song frame, normalised correlation 0..1) where a stretch of live audio matches the song,
/// starts searched in [lo, hi): the local peaks of the correlation, at least 0.3 s apart, best first. Unlike
/// `locate`, the runners-up are kept: a repetitive intro matches one beat off nearly as well as in the right place,
/// and the tracker (tracker.rs) keeps both until the music tells them apart.
pub fn locate_peaks(live: &[[f32; FP_BANDS]], song: &[[f32; FP_BANDS]], lo: i64, hi: Option<i64>, k: usize) -> Vec<(i64, f32)> {
    let n = live.len();
    if n < LOCATE_MIN_FRAMES || song.len() < n {
        return Vec::new();
    }
    let max_hi = (song.len() - n + 1) as i64;
    let hi = hi.map_or(max_hi, |h| h.min(max_hi));
    let lo = lo.max(0);
    if hi <= lo {
        return Vec::new();
    }
    let live_norm = live.iter().flat_map(|r| r.iter()).map(|v| (*v as f64).powi(2)).sum::<f64>().sqrt();
    // the song's energy over each window, from a running sum
    let mut csum = vec![0f64; song.len() + 1];
    for (i, r) in song.iter().enumerate() {
        csum[i + 1] = csum[i] + r.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
    }
    let c: Vec<f32> = (lo..hi).map(|k0| {
        let k = k0 as usize;
        let mut acc = 0f32;
        for (t, l) in live.iter().enumerate() {
            let s = &song[k + t];
            for b in 0..FP_BANDS {
                acc += s[b] * l[b];
            }
        }
        let norm = live_norm * (csum[k + n] - csum[k]).sqrt();
        if norm > 0.0 { (acc as f64 / norm) as f32 } else { 0.0 }
    }).collect();
    let mut order: Vec<usize> = (0..c.len()).collect();
    order.sort_by(|a, b| c[*b].total_cmp(&c[*a]));
    let mut peaks: Vec<(i64, f32)> = Vec::new();
    for i in order {
        if peaks.len() >= k || c[i] <= 0.0 {
            break;
        }
        let f = lo + i as i64;
        if peaks.iter().all(|(p, _)| (p - f).abs() > 30) {
            peaks.push((f, c[i]));
        }
    }
    peaks
}

/// How alike the song is to itself `lag` frames later, over `n` frames from frame `at` (normalised correlation
/// 0..1): high where a passage repeats at that distance (a repetitive intro a beat on, a chorus a verse on).
pub fn self_similarity(song: &[[f32; FP_BANDS]], at: i64, n: usize, lag: i64) -> f32 {
    let (a, b) = (at.max(0) as usize, (at + lag).max(0) as usize);
    if a + n > song.len() || b + n > song.len() {
        return 0.0;
    }
    let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
    for t in 0..n {
        for k in 0..FP_BANDS {
            let (x, y) = (song[a + t][k] as f64, song[b + t][k] as f64);
            ab += x * y;
            aa += x * x;
            bb += y * y;
        }
    }
    if aa > 0.0 && bb > 0.0 { (ab / (aa * bb).sqrt()) as f32 } else { 0.0 }
}

/// Median filter as torchcrepe.filter.median: over the values inside the signal and not NaN, taking the
/// lower middle value; NaN where the window has none.
pub fn nan_median_filter(x: &[f32], win: usize) -> Vec<f32> {
    let h = (win / 2) as i64;
    let n = x.len() as i64;
    (0..n).map(|i| {
        let mut w: Vec<f32> = ((i - h)..=(i + h)).filter(|&k| k >= 0 && k < n).map(|k| x[k as usize])
            .filter(|v| !v.is_nan()).collect();
        if w.is_empty() {
            return f32::NAN;
        }
        w.sort_by(|a, b| a.total_cmp(b));
        w[(w.len() - 1) / 2]
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skip_leaves_the_singing_check_where_it_was() {
        // the song leaking from speakers at -40 dB, you singing over it at -20 dB a third of the time; then the
        // silence of a skip over a quarter of the take
        let mut db: Vec<f32> = (0..3000).map(|i| if i % 3 == 0 { -20.0 } else { -40.0 }).collect();
        let bar = singing_gate(&db);
        assert!(bar > -40.0 && bar < -20.0, "{bar}");
        db.extend(std::iter::repeat(frame_db(&[], 0)).take(1000));
        assert_eq!(singing_gate(&db), bar);
        assert_eq!(singing_gate(&db[3000..]), f32::INFINITY, "nothing heard: nothing passes");
    }

    #[test]
    fn folding() {
        assert_eq!(fold(1250.0), 50.0);
        assert_eq!(fold(-650.0), 550.0);
        assert_eq!(credit(40.0), 1.0);
        assert_eq!(credit(-200.0), 0.0);
    }

    #[test]
    fn tolerant_error_forgives_timing_not_pitch() {
        // the singer holds 60 then 62; you sing the same 5 frames late, then 70 cents sharp
        let mut reference = vec![f32::NAN; 8];
        reference.extend(std::iter::repeat(60.0).take(50));
        reference.extend(std::iter::repeat(62.0).take(50));
        reference.extend(vec![f32::NAN; 8]);
        let mut usr = vec![60.0f32; 5];
        usr.extend(std::iter::repeat(60.0).take(45));
        usr.extend(std::iter::repeat(62.7).take(50));
        let e = tolerant_error(&reference, &usr, TOL, 50);
        assert!(e[..50].iter().all(|v| v.abs() < 1e-3));
        assert!(e[60..].iter().all(|v| (v - 70.0).abs() < 1e-2));
    }
}
