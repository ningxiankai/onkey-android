//! Lining lyrics up with the singer. LRCLIB's timings are often for a different recording of the song (a
//! remaster with a longer intro, the film version), so once OnKey knows when the singer actually sings
//! (the learned pitch line), each lyric line is moved to the phrase it belongs to:
//!   1. one overall shift (±15 s) that best puts line starts on phrase starts;
//!   2. each line then snaps to a phrase start within 0.6 s, if there is one, keeping lines in order;
//!      lines with none nearby move with their neighbours (interpolated), so a slow drift is followed too.
use crate::dsp::FRAME_S;

pub struct Aligned {
    pub lyrics: Vec<(f64, String)>,
    /// the overall shift applied, seconds
    pub shift: f64,
    /// share of lyric lines that start on a phrase start after the shift
    pub matched: f64,
    /// how well the lyrics fit the singing at the chosen shift (correlation), and at the best clearly different one
    pub fit: f64,
    pub runner_up: f64,
}

/// Phrase starts (seconds): the singer starts after at least 0.3 s of silence (gaps under 0.15 s ignored).
pub fn phrase_starts(voiced: &[bool]) -> Vec<f64> {
    let mut v = voiced.to_vec();
    // close short gaps
    let mut i = 0;
    while i < v.len() {
        if !v[i] {
            let j = (i..v.len()).find(|&k| v[k]).unwrap_or(v.len());
            if i > 0 && j < v.len() && j - i < 15 {
                v[i..j].iter_mut().for_each(|x| *x = true);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    let mut silent = 30usize;
    for (i, &x) in v.iter().enumerate() {
        if x {
            // a phrase must last 0.2 s to count (not a stray voiced frame)
            if silent >= 30 && v[i..(i + 20).min(v.len())].iter().filter(|b| **b).count() >= 15 {
                out.push(i as f64 * FRAME_S);
            }
            silent = 0;
        } else {
            silent += 1;
        }
    }
    out
}

fn nearest(starts: &[f64], t: f64) -> Option<f64> {
    let i = starts.partition_point(|&s| s < t);
    [i.checked_sub(1), Some(i)].into_iter().flatten().filter_map(|k| starts.get(k)).copied()
        .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
}

/// When the lyrics say someone is singing: each line from its start for a length that fits its words
/// (cut short by the next line), as song frames.
fn lyric_activity(lyrics: &[(f64, String)], n: usize, shift: f64) -> Vec<f32> {
    let mut l = vec![0f32; n];
    for (i, (t, text)) in lyrics.iter().enumerate() {
        let words = text.split_whitespace().count();
        if words == 0 {
            continue;
        }
        let next = lyrics.get(i + 1).map_or(f64::MAX, |x| x.0);
        let dur = (0.35 * words as f64 + 0.6).clamp(1.2, 6.0).min(next - t);
        let (a, b) = (((t + shift) / FRAME_S).round() as i64, ((t + shift + dur) / FRAME_S).round() as i64);
        for k in a.max(0)..b.min(n as i64) {
            l[k as usize] = 1.0;
        }
    }
    l
}

/// Correlation of lyric activity (shifted by `shift`) with singing, over frames [lo, hi).
fn corr(lyrics: &[(f64, String)], v: &[f32], shift: f64, lo: usize, hi: usize) -> f64 {
    let l = lyric_activity(lyrics, v.len(), shift);
    let n = (hi - lo) as f64;
    if n < 100.0 {
        return 0.0;
    }
    let (ml, mv) = (l[lo..hi].iter().map(|x| *x as f64).sum::<f64>() / n, v[lo..hi].iter().map(|x| *x as f64).sum::<f64>() / n);
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for k in lo..hi {
        let (x, y) = (l[k] as f64 - ml, v[k] as f64 - mv);
        sxy += x * y;
        sxx += x * x;
        syy += y * y;
    }
    if sxx <= 0.0 || syy <= 0.0 { 0.0 } else { sxy / (sxx * syy).sqrt() }
}

fn best_shift(lyrics: &[(f64, String)], v: &[f32], lo: usize, hi: usize, from: f64, to: f64, step: f64, center: f64) -> (f64, f64) {
    let mut best = (f64::MIN, center);
    let mut d = from;
    while d <= to + 1e-9 {
        // a slight preference for staying near `center`: equal evidence never moves the lyrics far
        let c = corr(lyrics, v, d, lo, hi) - 0.002 * (d - center).abs();
        if c > best.0 {
            best = (c, d);
        }
        d += step;
    }
    best
}

/// `voiced` covers song frames [0, voiced.len()). Returns None when the singing doesn't clearly match the
/// lyrics (then they're left as they are).
pub fn align(lyrics: &[(f64, String)], voiced: &[bool]) -> Option<Aligned> {
    let lines: Vec<usize> = (0..lyrics.len()).filter(|&i| lyrics[i].1.chars().any(|c| c.is_alphanumeric())).collect();
    if lines.len() < 4 {
        return None;
    }
    let v: Vec<f32> = voiced.iter().map(|b| if *b { 1.0 } else { 0.0 }).collect();
    if v.iter().all(|x| *x == 0.0) {
        return None;
    }
    let n = v.len();
    // 1. one overall shift, judged where there are lyrics
    let first = lyrics[lines[0]].0;
    let last = lyrics[*lines.last().unwrap()].0 + 6.0;
    let lo = (((first - 16.0) / FRAME_S).max(0.0)) as usize;
    let hi = (((last + 16.0) / FRAME_S) as usize).min(n);
    if hi <= lo + 1000 {
        return None;
    }
    let mut curve = Vec::new();
    let mut d = -15.0f64;
    while d <= 15.0 + 1e-9 {
        curve.push((corr(lyrics, &v, d, lo, hi) - 0.002 * d.abs(), d));
        d += 0.05;
    }
    let (c0, shift) = curve.iter().copied().max_by(|a, b| a.0.total_cmp(&b.0)).unwrap();
    // the best other peak of the fit, at least 1.5 s away: songs repeat their phrasing, so another shift can fit
    // almost as well; then the lyrics are left as they are
    let runner_up = (1..curve.len() - 1)
        .filter(|&k| curve[k].0 >= curve[k - 1].0 && curve[k].0 >= curve[k + 1].0 && (curve[k].1 - shift).abs() >= 1.5)
        .map(|k| curve[k].0).fold(f64::MIN, f64::max);
    let (_, shift) = best_shift(lyrics, &v, lo, hi, shift - 0.06, shift + 0.06, 0.01, shift);
    if !(c0 >= 0.3 && c0 >= runner_up + 0.08) {
        return None;
    }
    // 2. per line, the shift that best fits the 30 s around it (within 1.5 s of the overall one), smoothed over
    //    neighbouring lines: follows a recording that drifts from the lyrics' timing
    let local: Vec<f64> = lyrics.iter().map(|(t, _)| {
        let a = (((t + shift - 15.0) / FRAME_S).max(0.0)) as usize;
        let b = (((t + shift + 15.0) / FRAME_S) as usize).min(n);
        if b <= a + 1000 { return shift; }
        best_shift(lyrics, &v, a, b, shift - 1.5, shift + 1.5, 0.02, shift).1
    }).collect();
    let smooth: Vec<f64> = (0..local.len()).map(|i| {
        let mut w: Vec<f64> = local[i.saturating_sub(3)..(i + 4).min(local.len())].to_vec();
        w.sort_by(|a, b| a.total_cmp(b));
        w[w.len() / 2]
    }).collect();
    let mut times: Vec<f64> = lyrics.iter().zip(&smooth).map(|((t, _), d)| t + d).collect();
    // 3. a line that starts within 0.6 s of a phrase start goes there (shown 0.2 s ahead of the voice)
    let starts = phrase_starts(voiced);
    let mut on_phrase = 0;
    for &i in &lines {
        if let Some(p) = nearest(&starts, times[i]) {
            if (p - 0.2 - times[i]).abs() <= 0.6 {
                times[i] = p - 0.2;
                on_phrase += 1;
            }
        }
    }
    for i in 1..times.len() {
        if times[i] < times[i - 1] {
            times[i] = times[i - 1];
        }
    }
    Some(Aligned { lyrics: times.into_iter().zip(lyrics.iter().map(|l| l.1.clone())).map(|(t, s)| ((t * 100.0).round() / 100.0, s)).collect(),
                   shift, matched: on_phrase as f64 / lines.len() as f64, fit: c0, runner_up })
}

/// When the lead singer is singing: the separated lead vocal is within 9 dB of the whole mix (over 100 ms).
/// Separation leaves a little music in the vocal, so its own loudness isn't enough; relative to the mix it
/// jumps when someone actually sings. Both 16 kHz mono, same timeline; one value per 10 ms frame.
pub fn singing(vocal16: &[f32], mix16: &[f32]) -> Vec<bool> {
    let n = vocal16.len().min(mix16.len()) / 160;
    let db = |x: &[f32], i: usize| {
        let f = &x[i * 160..(i + 1) * 160];
        10.0 * (f.iter().map(|v| v * v).sum::<f32>() / 160.0 + 1e-10).log10()
    };
    let r: Vec<f32> = (0..n).map(|i| db(vocal16, i) - db(mix16, i)).collect();
    let loud: Vec<bool> = (0..n).map(|i| db(mix16, i) > -60.0).collect();
    (0..n).map(|i| {
        let (a, b) = (i.saturating_sub(5), (i + 5).min(n));
        loud[i] && r[a..b].iter().sum::<f32>() / (b - a) as f32 > -9.0
    }).collect()
}
