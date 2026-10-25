//! The music's shape for the Sing page's light ribbon (drawn while no notes are known: a blind take, or a song
//! being learned): 60 frames a second of how loud the music is overall and in four log-spaced bands, and its
//! beat. Each band is in dB, auto-gained against its own slowly falling peak and smoothed with a fast attack
//! (25 ms) and a slower release (220 ms); a beat is the bass rising sharply. Cheap: one 2048-point real FFT per
//! frame (about 0.1% of a core).
use std::sync::Arc;

use realfft::{num_complex::Complex32, RealFftPlanner, RealToComplex};

/// Frames per second.
pub const FPS: f64 = 60.0;
const N: usize = 2048;
/// The bands, in Hz: bass, low mids, mids, highs.
const BANDS: [(f32, f32); 4] = [(30.0, 120.0), (120.0, 500.0), (500.0, 2000.0), (2000.0, 10000.0)];
/// dB below a band's running peak that counts as silence.
const RANGE: f32 = 30.0;

/// One frame: [rms, bass, low, mid, high, beat, onset]. The first six are 0..1; `beat` jumps up on a beat and
/// decays over ~0.2 s; `onset` is how sharply the bass rose (dB) on the frame a beat starts, else 0.
pub type Frame = [f32; 7];

pub struct Viz {
    rate: f64,
    fft: Arc<dyn RealToComplex<f32>>,
    win: Vec<f32>,
    ring: Vec<f32>,
    pos: usize,
    /// samples received
    n: u64,
    /// the sample (count) at which the next frame's window ends
    next_end: f64,
    input: Vec<f32>,
    spec: Vec<Complex32>,
    scratch: Vec<Complex32>,
    bins: [(usize, usize); 4],
    env: [f32; 4],
    peak: [f32; 4],
    rms: f32,
    rms_peak: f32,
    beat: f32,
    last_beat: f64,
    recent: [f32; 4],
    /// frames not sent yet, and the take frame (10 ms) of the first of them
    out: Vec<Frame>,
    out_at: f64,
}

impl Viz {
    pub fn new(rate: f64) -> Viz {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N);
        let (input, spec, scratch) = (fft.make_input_vec(), fft.make_output_vec(), fft.make_scratch_vec());
        // Blackman, as Web Audio's analyser (so levels match the design prototype's)
        let win = (0..N).map(|i| {
            let x = 2.0 * std::f32::consts::PI * i as f32 / N as f32;
            0.42 - 0.5 * x.cos() + 0.08 * (2.0 * x).cos()
        }).collect();
        let hz = rate as f32 / N as f32;
        let bins = BANDS.map(|(lo, hi)| (((lo / hz).round() as usize).max(1), ((hi / hz).round() as usize).min(N / 2)));
        Viz {
            rate, fft, win, ring: vec![0.0; N], pos: 0, n: 0, next_end: rate / FPS, input, spec, scratch, bins,
            env: [0.0; 4], peak: [-70.0; 4], rms: 0.0, rms_peak: -50.0, beat: 0.0, last_beat: -9.0, recent: [-120.0; 4],
            out: Vec::new(), out_at: 0.0,
        }
    }

    /// Mono music, as it arrives (the take's own samples, skips included as silence, so frames keep the take's clock).
    pub fn push(&mut self, mono: &[f32]) {
        for &x in mono {
            self.ring[self.pos] = x;
            self.pos = (self.pos + 1) % N;
            self.n += 1;
            if self.n as f64 >= self.next_end {
                self.next_end += self.rate / FPS;
                self.frame();
            }
        }
        // nobody collecting (a long skip, or no window): keep at most 2 s
        let keep = (2.0 * FPS) as usize;
        if self.out.len() > keep {
            let drop = self.out.len() - keep;
            self.out.drain(..drop);
            self.out_at += drop as f64 * 100.0 / FPS;
        }
    }

    /// The frames since the last call: the take frame (10 ms units) of the first, and the frames, 1/60 s apart.
    pub fn take_frames(&mut self) -> Option<(f64, Vec<Frame>)> {
        if self.out.is_empty() {
            return None;
        }
        let at = self.out_at;
        let frames = std::mem::take(&mut self.out);
        self.out_at += frames.len() as f64 * 100.0 / FPS;
        Some((at, frames))
    }

    fn frame(&mut self) {
        let dt = 1.0 / FPS as f32;
        let t = self.n as f64 / self.rate;
        // the frame belongs to the middle of its window
        if self.out.is_empty() {
            self.out_at = (self.n as f64 - N as f64 / 2.0) / self.rate * 100.0;
        }
        let (a, b) = self.ring.split_at(self.pos);
        let mut sq = 0.0f32;
        for (i, &x) in b.iter().chain(a.iter()).enumerate() {
            self.input[i] = x * self.win[i];
            sq += x * x;
        }
        if self.fft.process_with_scratch(&mut self.input, &mut self.spec, &mut self.scratch).is_err() {
            return;
        }
        let follow = |cur: f32, x: f32| cur + (x - cur) * (1.0 - (-dt / if x > cur { 0.025 } else { 0.22 }).exp());
        let norm = 1.0 / (N as f32 * N as f32);
        let mut bass_db = -120.0;
        for i in 0..4 {
            let (b0, b1) = self.bins[i];
            let p: f32 = self.spec[b0..=b1].iter().map(|c| c.norm_sqr() * norm).sum::<f32>() / (b1 - b0 + 1) as f32;
            let db = 10.0 * (p + 1e-12).log10();
            if i == 0 {
                bass_db = db;
            }
            self.peak[i] = db.max(self.peak[i] - dt * 1.5).max(-75.0);
            self.env[i] = follow(self.env[i], ((db - (self.peak[i] - RANGE)) / RANGE).clamp(0.0, 1.0));
        }
        let rdb = 10.0 * (sq / N as f32 + 1e-12).log10();
        self.rms_peak = rdb.max(self.rms_peak - dt * 1.5).max(-60.0);
        self.rms = follow(self.rms, ((rdb - (self.rms_peak - 36.0)) / 36.0).clamp(0.0, 1.0));
        // a beat: the bass rising fast (4.5 dB over its lowest in the last ~70 ms, that low floored at the bottom
        // of its range), loud enough, and not within 0.22 s of the last
        let low = self.recent.iter().copied().fold(f32::INFINITY, f32::min).max(self.peak[0] - RANGE);
        let rise = bass_db - low;
        self.recent.rotate_left(1);
        self.recent[3] = bass_db;
        let mut onset = 0.0;
        if rise > 4.5 && t - self.last_beat > 0.22 && self.env[0] > 0.35 {
            self.beat = self.beat.max((rise / 16.0).clamp(0.4, 1.0));
            self.last_beat = t;
            onset = rise;
        }
        self.beat *= (-dt / 0.2).exp();
        self.out.push([self.rms, self.env[0], self.env[1], self.env[2], self.env[3], self.beat, onset]);
    }
}

/// A batch of frames as the window gets it: {"type": "viz", "version", "f0": take frame of the first, "fps",
/// "fr": [[rms, bass, low, mid, high, beat, onset], ...]} (two decimals; onset one).
pub fn event(version: u64, f0: f64, frames: &[Frame]) -> serde_json::Value {
    let r2 = |v: f32| (v * 100.0).round() / 100.0;
    let fr: Vec<[f32; 7]> = frames.iter().map(|f| [r2(f[0]), r2(f[1]), r2(f[2]), r2(f[3]), r2(f[4]), r2(f[5]), (f[6] * 10.0).round() / 10.0]).collect();
    serde_json::json!({"type": "viz", "version": version, "f0": (f0 * 100.0).round() / 100.0, "fps": FPS, "fr": fr})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: f64, secs: f64, f: f64, amp: f32) -> Vec<f32> {
        (0..(rate * secs) as usize).map(|i| amp * (2.0 * std::f64::consts::PI * f * i as f64 / rate).sin() as f32).collect()
    }

    #[test]
    fn sixty_frames_a_second_on_the_take_clock() {
        let mut v = Viz::new(48_000.0);
        v.push(&tone(48_000.0, 1.0, 440.0, 0.3));
        let (f0, frames) = v.take_frames().unwrap();
        assert_eq!(frames.len(), 60);
        // the first frame's window ends at 1/60 s; it belongs to the window's middle
        assert!((f0 - (800.0 - 1024.0) / 480.0).abs() < 1e-6, "{f0}");
        v.push(&tone(48_000.0, 0.5, 440.0, 0.3));
        let (f1, more) = v.take_frames().unwrap();
        assert_eq!(more.len(), 30);
        assert!((f1 - (f0 + 100.0)).abs() < 1e-6);
        assert!(v.take_frames().is_none());
    }

    #[test]
    fn a_tone_lights_its_own_band() {
        for (f, band) in [(60.0, 0), (250.0, 1), (1000.0, 2), (5000.0, 3)] {
            let mut v = Viz::new(44_100.0);
            // silence first, so the other bands' peaks sit at the floor, then the tone
            v.push(&vec![0.0; 44_100]);
            v.push(&tone(44_100.0, 1.0, f, 0.3));
            let (_, fr) = v.take_frames().unwrap();
            let last = fr.last().unwrap();
            assert!(last[1 + band] > 0.9, "{f} Hz: {last:?}");
            for b in 0..4 {
                if b != band {
                    assert!(last[1 + b] < last[1 + band], "{f} Hz: {last:?}");
                }
            }
            assert!(last[0] > 0.5);
        }
    }

    #[test]
    fn kicks_are_beats() {
        // a kick (a 60 Hz burst decaying over 0.2 s) every half second, over a quiet pad
        let rate = 48_000.0;
        let mut x = tone(rate, 4.0, 330.0, 0.02);
        for k in 0..8 {
            let s0 = (k as f64 * 0.5 * rate) as usize;
            for i in 0..(0.25 * rate) as usize {
                let t = i as f64 / rate;
                x[s0 + i] += (0.8 * (-t / 0.06).exp() * (2.0 * std::f64::consts::PI * 60.0 * t).sin()) as f32;
            }
        }
        let mut v = Viz::new(rate);
        let mut fr = Vec::new();
        for piece in x.chunks(4800) {
            v.push(piece);
            fr.extend(v.take_frames().map(|(_, f)| f).unwrap_or_default());
        }
        let beats: Vec<usize> = fr.iter().enumerate().filter(|(_, f)| f[6] > 0.0).map(|(i, _)| i).collect();
        assert_eq!(beats.len(), 8, "{beats:?}");
        for (k, i) in beats.iter().enumerate() {
            // within a few frames of each kick (the window is 43 ms long)
            assert!((*i as f64 - k as f64 * 30.0).abs() <= 3.0, "{beats:?}");
        }
    }

    #[test]
    fn costs_little() {
        // under 1% of a core in a release build (`cargo test --release --lib viz -- --nocapture` prints it);
        // a debug build is several times slower, so it's only checked loosely there
        let rate = 48_000.0;
        let x = tone(rate, 10.0, 220.0, 0.2);
        let mut v = Viz::new(rate);
        let t0 = std::time::Instant::now();
        v.push(&x);
        let per_sec = t0.elapsed().as_secs_f64() / 10.0;
        println!("viz: {:.3} ms per second of music ({:.3}% of a core)", per_sec * 1e3, per_sec * 100.0);
        assert!(per_sec < if cfg!(debug_assertions) { 0.2 } else { 0.01 });
    }
}
