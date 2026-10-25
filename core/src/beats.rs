//! The beat of a learned song, for the dancer: from the song's stored onset envelope (the music fingerprint,
//! 10 ms frames), the tempo by autocorrelation (weighted towards 120 BPM), then the beats by dynamic
//! programming (Ellis, 2007: each beat on a strong onset, one beat period after the last), the downbeats
//! (every 4th beat, the phase with the most low-band onsets), and how energetic the music is at each beat.
use crate::dsp::FP_BANDS;

pub struct Beats {
    /// song frames (10 ms) of each beat
    pub at: Vec<i64>,
    /// beats at[downbeat], at[downbeat + 4], … start bars
    pub downbeat: usize,
    pub bpm: f32,
    /// 0..1 per beat: how busy the music is around it (quiet verse ≈ low, chorus ≈ high)
    pub energy: Vec<f32>,
}

pub fn track(onset: &[[f32; FP_BANDS]]) -> Option<Beats> {
    let n = onset.len();
    if n < 2000 {
        return None;
    }
    let raw: Vec<f32> = onset.iter().map(|r| r.iter().map(|v| v.max(0.0)).sum::<f32>()).collect();
    let mean = raw.iter().sum::<f32>() / n as f32;
    let sd = (raw.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n as f32).sqrt().max(1e-6);
    let o: Vec<f32> = raw.iter().map(|v| (v - mean) / sd).collect();
    // tempo: autocorrelation over 50-200 BPM, weighted by a log-Gaussian around 120 BPM (one octave wide)
    let mut best = (0usize, f32::NEG_INFINITY);
    for lag in 30..=120 {
        let ac: f32 = (lag..n).map(|t| o[t] * o[t - lag]).sum::<f32>() / (n - lag) as f32;
        let w = (-0.5 * ((lag as f32 / 50.0).log2() / 1.0).powi(2)).exp();
        if ac * w > best.1 {
            best = (lag, ac * w);
        }
    }
    let p = best.0 as f32;
    // beats: score[t] = onset strength + best earlier beat roughly one period back
    let alpha = 100.0;
    let mut score = vec![0f32; n];
    let mut back = vec![usize::MAX; n];
    for t in 0..n {
        let (lo, hi) = ((t as f32 - 2.0 * p).round().max(0.0) as usize, (t as f32 - p / 2.0).round().max(0.0) as usize);
        let mut b = (usize::MAX, 0f32);
        for tau in lo..hi.min(t) {
            let pen = -alpha * (((t - tau) as f32 / p).ln()).powi(2);
            let v = score[tau] + pen;
            if b.0 == usize::MAX || v > b.1 {
                b = (tau, v);
            }
        }
        score[t] = o[t] + if b.0 == usize::MAX { 0.0 } else { b.1.max(0.0) };
        back[t] = if b.1 > 0.0 { b.0 } else { usize::MAX };
    }
    // the last beat: the best score in the final period
    let tail = n.saturating_sub(p as usize * 2);
    let mut t = (tail..n).max_by(|a, b| score[*a].total_cmp(&score[*b]))?;
    let mut at = vec![t as i64];
    while back[t] != usize::MAX {
        t = back[t];
        at.push(t as i64);
    }
    at.reverse();
    // downbeats: the phase (of 4) with the most bass/kick onsets
    let low: Vec<f32> = onset.iter().map(|r| r[0].max(0.0) + r[1].max(0.0)).collect();
    let downbeat = (0..4).max_by(|a, b| {
        let s = |ph: usize| at.iter().skip(ph).step_by(4).map(|&f| low[f as usize]).sum::<f32>();
        s(*a).total_cmp(&s(*b))
    }).unwrap_or(0);
    // energy: onset activity over ±2 s, scaled to the song's own range
    let mut cs = vec![0f64; n + 1];
    for i in 0..n {
        cs[i + 1] = cs[i] + raw[i] as f64;
    }
    let e: Vec<f32> = at.iter().map(|&f| {
        let (a, b) = ((f - 200).max(0) as usize, ((f + 200) as usize).min(n));
        ((cs[b] - cs[a]) / (b - a).max(1) as f64) as f32
    }).collect();
    let mut s = e.clone();
    s.sort_by(|a, b| a.total_cmp(b));
    let (lo, hi) = (s[s.len() / 10], s[s.len() * 9 / 10].max(s[s.len() / 10] + 1e-6));
    let energy = e.iter().map(|v| ((v - lo) / (hi - lo)).clamp(0.0, 1.0)).collect();
    Some(Beats { at, downbeat, bpm: 6000.0 / p, energy })
}
