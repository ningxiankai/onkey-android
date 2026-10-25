//! Turns a pitch line into steady note bars as frames arrive, the way karaoke games draw notes.
//! A note is shown once `confirm` frames agree on a semitone, then holds that semitone (wobble, vibrato and
//! slides within `hold` cents are ignored) until the pitch sits on a different note for `move_` frames, or
//! the singer stops for more than `gap` frames. Finished notes are never redrawn. (Port of live.NoteTracker.)

pub type Bar = (i64, i64, i32); // start frame, end frame, MIDI note

pub struct NoteTracker {
    confirm: usize,
    hold: f32,
    move_: usize,
    gap: i64,
    min_len: i64,
    pub notes: Vec<Bar>,
    cur: Option<(i64, i64, i32)>, // start, last voiced, midi
    cand: Vec<(i64, f32)>,
    off: Vec<i64>,
}

impl Default for NoteTracker {
    fn default() -> Self {
        NoteTracker { confirm: 8, hold: 70.0, move_: 12, gap: 8, min_len: 12, notes: Vec::new(), cur: None,
                      cand: Vec::new(), off: Vec::new() }
    }
}

impl NoteTracker {
    /// A tracker with its own thresholds (frames and cents), e.g. for the notes of a run within one syllable.
    pub fn with(confirm: usize, hold: f32, move_: usize, gap: i64, min_len: i64) -> NoteTracker {
        NoteTracker { confirm, hold, move_, gap, min_len, ..NoteTracker::default() }
    }

    fn close(&mut self, end: i64) {
        if let Some((a, _, m)) = self.cur.take() {
            if end - a >= self.min_len {
                self.notes.push((a, end, m));
            }
        }
        self.off.clear();
    }

    pub fn feed(&mut self, frames: &[f32], start: i64) {
        for (j, &m) in frames.iter().enumerate() {
            let i = start + j as i64;
            if let Some((_, last, note)) = self.cur {
                if m.is_nan() {
                    if i - last > self.gap {
                        self.close(last + 1);
                    }
                    continue;
                }
                if (100.0 * (m - note as f32)).abs() <= self.hold {
                    self.cur.as_mut().unwrap().1 = i;
                    self.off.clear();
                    continue;
                }
                self.off.push(i);
                if self.off.len() >= self.move_ {
                    let first = self.off[0];
                    self.close(first);
                    self.cand = (first..=i).filter(|&k| k >= start)
                        .map(|k| (k, frames[(k - start) as usize])).filter(|(_, v)| !v.is_nan()).collect();
                }
                continue;
            }
            if m.is_nan() {
                if let Some(&(last, _)) = self.cand.last() {
                    if i - last > self.gap / 2 {
                        self.cand.clear();
                    }
                }
                continue;
            }
            self.cand.push((i, m));
            if self.cand.len() >= self.confirm {
                let tail = &self.cand[self.cand.len() - self.confirm..];
                let (lo, hi) = tail.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), (_, v)| (a.min(*v), b.max(*v)));
                if hi - lo <= 1.0 {
                    let mut vals: Vec<f32> = tail.iter().map(|(_, v)| *v).collect();
                    let med = crate::dsp::median(&mut vals);
                    self.cur = Some((tail[0].0, i, med.round() as i32));
                    self.cand.clear();
                }
            }
        }
    }

    /// Close the open note at the end of the line.
    pub fn finish(&mut self) {
        if let Some((_, last, _)) = self.cur {
            self.close(last + 1);
        }
    }

    /// Steady notes for a whole pitch line.
    pub fn bars(line: &[f32]) -> Vec<Bar> {
        let mut t = NoteTracker::default();
        t.feed(line, 0);
        t.finish();
        t.notes
    }
}

/// Clean a learned pitch line before it becomes note bars. The pitch tracker sometimes locks onto an octave
/// below (or above) the voice, and separation lets some bass and instruments through; both show up as notes far
/// from where the singer is. (Seen on real songs: Matilda's and The Funeral's stray bars were nearly all an
/// octave or more under the singer.)
///   1. the song's register (median pitch): frames 9+ semitones away move by octaves towards it; >12 dropped
///   2. the local register (median over ±3 s): the same at 7 semitones, and frames still >7 away dropped
///   3. voiced stretches shorter than 0.1 s dropped
pub fn clean_line(line: &[f32]) -> Vec<f32> {
    let mut x = line.to_vec();
    let n = x.len();
    let voiced = |x: &[f32]| -> Vec<usize> { (0..x.len()).filter(|&i| !x[i].is_nan()).collect() };
    let idx = voiced(&x);
    if idx.len() < 50 {
        return x;
    }
    let mut v: Vec<f32> = idx.iter().map(|&i| x[i]).collect();
    let g = crate::dsp::median(&mut v);
    for m in x.iter_mut().filter(|m| !m.is_nan()) {
        let d = *m - g;
        if d.abs() > 9.0 {
            *m -= 12.0 * (d / 12.0).round();
        }
        if (*m - g).abs() > 12.0 {
            *m = f32::NAN;
        }
    }
    // local register, every 10 frames from the voiced frames within ±3 s, interpolated
    let idx = voiced(&x);
    let mut anchors: Vec<(usize, f32)> = Vec::new();
    for i in (0..n).step_by(10) {
        let a = idx.partition_point(|&k| k + 300 < i);
        let b = idx.partition_point(|&k| k <= i + 300);
        if b - a >= 20 {
            let mut w: Vec<f32> = idx[a..b].iter().map(|&k| x[k]).collect();
            anchors.push((i, crate::dsp::median(&mut w)));
        }
    }
    if anchors.len() >= 2 {
        let mut j = 0;
        for i in 0..n {
            if x[i].is_nan() {
                continue;
            }
            while j + 1 < anchors.len() && anchors[j + 1].0 <= i {
                j += 1;
            }
            let reg = if i <= anchors[0].0 { anchors[0].1 } else if j + 1 >= anchors.len() { anchors[j].1 } else {
                let (a, b) = (anchors[j], anchors[j + 1]);
                a.1 + (b.1 - a.1) * (i - a.0) as f32 / (b.0 - a.0) as f32
            };
            let d = x[i] - reg;
            if d.abs() > 7.0 {
                x[i] -= 12.0 * (d / 12.0).round();
            }
            if (x[i] - reg).abs() > 7.0 {
                x[i] = f32::NAN;
            }
        }
    }
    // drop blips
    let mut i = 0;
    while i < n {
        if x[i].is_nan() {
            i += 1;
            continue;
        }
        let j = (i..n).find(|&k| x[k].is_nan()).unwrap_or(n);
        if j - i < 10 {
            x[i..j].iter_mut().for_each(|v| *v = f32::NAN);
        }
        i = j;
    }
    x
}
