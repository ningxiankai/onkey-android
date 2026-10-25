//! The bleed guard: when you sing without headphones, the mic also hears the song (with its singer) from your
//! speakers. OnKey knows exactly what is being played, on the same clock as the mic, so it learns how that
//! sound reaches the mic (delay, the room's echo, the speaker's colour) and subtracts it, as video calls cancel
//! echo. With headphones there's nothing to learn and the mic passes through unchanged.
//!
//! A partitioned-block frequency-domain adaptive filter (constrained, one partition re-constrained per block):
//! blocks of 256 samples at 16 kHz, 16 partitions = 256 ms of echo path. Some outputs put the echo later than
//! that (~290 ms), so the echo's delay is measured as it plays and the music is fed to the filter that late.
use realfft::{num_complex::Complex32, ComplexToReal, RealFftPlanner, RealToComplex};
use std::collections::VecDeque;
use std::sync::Arc;

const B: usize = 256;
const N: usize = 2 * B;
const P: usize = 16;
const BINS: usize = B + 1;
/// echo delay search: over the last 4 s, every 2 s, up to 800 ms; the filter keeps 48 ms before the echo
const HIST: usize = 4 * 16_000;
const CHECK: usize = 2 * 16_000;
const MAX_LAG: usize = 800 * 16;
const MARGIN: usize = 48 * 16;

pub struct Canceller {
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    prev_ref: Vec<f32>,
    xf: VecDeque<Vec<Complex32>>,
    w: Vec<Vec<Complex32>>,
    pw: Vec<f32>,
    next_constrain: usize,
    mic_in: Vec<f32>,
    mu: f32,
    /// slow averages of the mic's energy and of the echo the filter predicts: the cleaned signal is used only
    /// while the song is clearly reaching the mic (otherwise, e.g. with headphones, the mic passes untouched)
    mic_pow: f32,
    echo_pow: f32,
    /// blocks in a row where the output came out louder than the mic (the filter diverging)
    louder: u32,
    /// the music is fed to the filter this many samples late: the echo can arrive later than the filter reaches
    /// (some outputs ~290 ms), so the bulk of the delay is measured (GCC-PHAT) and taken off the reference
    ref_delay: usize,
    ref_hist: VecDeque<f32>,
    /// samples of mic processed so far, and of music received (ref_hist ends at music sample `got`)
    pos: usize,
    got: usize,
    mic_hist: VecDeque<f32>,
    next_check: usize,
    last_lag: Option<usize>,
    gcc: Option<(Arc<dyn RealToComplex<f32>>, Arc<dyn ComplexToReal<f32>>)>,
}

impl Default for Canceller {
    fn default() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        Canceller {
            fft: planner.plan_fft_forward(N), ifft: planner.plan_fft_inverse(N), prev_ref: vec![0.0; B],
            xf: (0..P).map(|_| vec![Complex32::new(0.0, 0.0); BINS]).collect(),
            w: (0..P).map(|_| vec![Complex32::new(0.0, 0.0); BINS]).collect(),
            pw: vec![0.0; BINS], next_constrain: 0, mic_in: Vec::new(), mu: 0.35,
            mic_pow: 0.0, echo_pow: 0.0, louder: 0,
            ref_delay: 0, ref_hist: VecDeque::new(), pos: 0, got: 0, mic_hist: VecDeque::new(), next_check: HIST,
            last_lag: None, gcc: None,
        }
    }
}

impl Canceller {
    /// New music (what's being played) and mic samples, both 16 kHz on the same clock. Appends the cleaned mic,
    /// in blocks of 256 samples (so up to 16 ms later than it arrives).
    pub fn process(&mut self, music: &[f32], mic: &[f32], out: &mut Vec<f32>) {
        self.ref_hist.extend(music.iter().copied());
        self.got += music.len();
        self.mic_in.extend_from_slice(mic);
        // the reference block for mic samples [pos, pos + B) is music [pos - delay, pos + B - delay)
        while self.mic_in.len() >= B && self.got >= self.pos + B {
            let start = self.pos as i64 - self.ref_delay as i64;
            let base = self.got - self.ref_hist.len();          // music sample index of ref_hist[0]
            let x: Vec<f32> = (0..B as i64).map(|i| {
                let j = start + i;
                if j < base as i64 { 0.0 } else { self.ref_hist[(j - base as i64) as usize] }
            }).collect();
            let d: Vec<f32> = self.mic_in.drain(..B).collect();
            self.mic_hist.extend(d.iter().copied());
            while self.mic_hist.len() > HIST { self.mic_hist.pop_front(); }
            self.block(&x, &d, out);
            self.pos += B;
            if self.pos >= self.next_check {
                self.next_check = self.pos + CHECK;
                self.check_delay();
            }
        }
        // keep enough music for the delay search and the delayed feed
        let keep = HIST + MAX_LAG + B + (self.got - self.pos.min(self.got));
        while self.ref_hist.len() > keep { self.ref_hist.pop_front(); }
    }

    /// Where the echo is: the lag of the mic behind the music (GCC-PHAT over the last 4 s). Taken off the
    /// reference once two checks in a row agree, if it's beyond what the filter reaches comfortably.
    fn check_delay(&mut self) {
        if self.mic_hist.len() < HIST || self.pos < HIST + MAX_LAG { return; }
        let n = 1usize << 18;
        let (f, fi) = self.gcc.get_or_insert_with(|| {
            let mut pl = RealFftPlanner::<f32>::new();
            (pl.plan_fft_forward(n), pl.plan_fft_inverse(n))
        }).clone();
        // mic: the last 4 s; music: the same 4 s plus MAX_LAG before
        let base = self.got - self.ref_hist.len();
        let m0 = self.pos - HIST;
        let r0 = m0 - MAX_LAG;
        if r0 < base { return; }
        let mut a = vec![0f32; n];
        for (i, v) in self.mic_hist.iter().enumerate() { a[i] = *v; }
        let mut b = vec![0f32; n];
        for i in 0..HIST + MAX_LAG { b[i] = self.ref_hist[r0 - base + i]; }
        if b.iter().map(|v| v * v).sum::<f32>() < 1e-6 { return; }
        let mut af = f.make_output_vec();
        let mut bf = f.make_output_vec();
        f.process(&mut a, &mut af).unwrap();
        f.process(&mut b, &mut bf).unwrap();
        let mut xf: Vec<Complex32> = af.iter().zip(&bf).map(|(p, q)| { let c = p * q.conj(); c / (c.norm() + 1e-12) }).collect();
        xf[0].im = 0.0;
        let last = xf.len() - 1;
        xf[last].im = 0.0;
        let mut x = fi.make_output_vec();
        fi.process(&mut xf, &mut x).unwrap();
        // mic[i] ~ music[r0 + i + MAX_LAG - lag]: correlation of a against b shifted; peak at circular index
        // (n - (MAX_LAG - lag)) % n for lag in 0..=MAX_LAG
        let at = |lag: usize| x[(n - (MAX_LAG - lag)) % n];
        let peak = (0..=MAX_LAG).map(at).fold(f32::MIN, f32::max);
        let mean = x.iter().map(|v| v.abs()).sum::<f32>() / n as f32;
        if peak < 12.0 * mean { self.last_lag = None; return; }   // no clear echo
        // while the filter already reaches a clear copy of the music, leave the delay alone (a voice singing the
        // very same line, as the simulated singers do, can line up later and more strongly than the echo); a
        // real echo out of reach leaves only spurious bumps there (8-18x the mean, against 50-900x for an echo)
        let reach = P * B * 3 / 4;
        if (self.ref_delay..=(self.ref_delay + reach).min(MAX_LAG)).any(|l| at(l) >= 30.0 * mean) { self.last_lag = None; return; }
        // otherwise the echo is the earliest strong copy
        let Some(lag) = (0..=MAX_LAG).find(|&l| at(l) >= (0.5 * peak).max(30.0 * mean)) else { return };
        let agree = self.last_lag.is_some_and(|l| l.abs_diff(lag) <= 32);
        self.last_lag = Some(lag);
        if !agree { return; }
        let want = lag.saturating_sub(MARGIN);
        if want.abs_diff(self.ref_delay) > B {
            self.ref_delay = want;
            for wp in self.w.iter_mut() { wp.iter_mut().for_each(|v| *v = Complex32::new(0.0, 0.0)); }
            self.echo_pow = 0.0;
        }
    }

    pub fn ref_delay_ms(&self) -> usize { self.ref_delay / 16 }

    /// Is the song reaching the mic? The predicted echo is within 25 dB of what the mic hears.
    pub fn bleeding(&self) -> bool {
        self.echo_pow > 0.003 * self.mic_pow && self.echo_pow > 1e-9
    }

    fn block(&mut self, x: &[f32], d: &[f32], out: &mut Vec<f32>) {
        // spectrum of the last two blocks of music
        let mut buf: Vec<f32> = self.prev_ref.iter().chain(x).copied().collect();
        let mut xf0 = self.fft.make_output_vec();
        self.fft.process(&mut buf, &mut xf0).unwrap();
        self.prev_ref.copy_from_slice(x);
        self.xf.pop_back();
        self.xf.push_front(xf0);
        // the echo the filter predicts
        let mut yf = vec![Complex32::new(0.0, 0.0); BINS];
        for p in 0..P {
            for k in 0..BINS {
                yf[k] += self.w[p][k] * self.xf[p][k];
            }
        }
        let mut y = self.ifft.make_output_vec();
        yf[0].im = 0.0;
        yf[B].im = 0.0;
        self.ifft.process(&mut yf, &mut y).unwrap();
        let e: Vec<f32> = (0..B).map(|i| d[i] - y[B + i] / N as f32).collect();
        let ep = (0..B).map(|i| (y[B + i] / N as f32).powi(2)).sum::<f32>() / B as f32;
        let mp = d.iter().map(|v| v * v).sum::<f32>() / B as f32;
        self.echo_pow = 0.98 * self.echo_pow + 0.02 * ep;     // ~0.8 s
        self.mic_pow = 0.98 * self.mic_pow + 0.02 * mp;
        // a canceller only ever removes sound: if the output is louder than the mic, the filter is diverging
        // (e.g. music starting from silence), so pass the mic through, and start over if it keeps happening
        let rp = e.iter().map(|v| v * v).sum::<f32>() / B as f32;
        let diverging = rp > 2.0 * mp + 1e-12;
        self.louder = if diverging { self.louder + 1 } else { 0 };
        if self.louder >= 3 {
            for wp in self.w.iter_mut() {
                wp.iter_mut().for_each(|v| *v = Complex32::new(0.0, 0.0));
            }
            self.echo_pow = 0.0;
            self.louder = 0;
        }
        out.extend_from_slice(if self.bleeding() && !diverging { &e } else { d });
        if diverging {
            return;                                 // don't learn from a diverged block
        }
        // learn: normalised gradient step for every partition
        let mut eb = vec![0f32; B];
        eb.extend_from_slice(&e);
        let mut ef = self.fft.make_output_vec();
        self.fft.process(&mut eb, &mut ef).unwrap();
        let mut mean = 0f32;
        for k in 0..BINS {
            // the music's level per bin: never under this block's (music starting from silence would otherwise
            // make the steps huge and the filter diverge)
            self.pw[k] = (0.9 * self.pw[k] + 0.1 * self.xf[0][k].norm_sqr()).max(0.5 * self.xf[0][k].norm_sqr());
            mean += self.pw[k];
        }
        mean /= BINS as f32;
        if mean < 1e-9 {
            return;                                 // no music playing: nothing to learn from
        }
        let delta = 0.01 * mean + 1e-3;
        let mu = self.mu;
        for p in 0..P {
            for k in 0..BINS {
                let g = self.xf[p][k].conj() * ef[k] * (mu / (P as f32 * self.pw[k] + delta));
                self.w[p][k] += g;
            }
        }
        // keep the filter causal and 256 samples per partition (one partition per block)
        let p = self.next_constrain;
        self.next_constrain = (p + 1) % P;
        let mut wp = self.w[p].clone();
        wp[0].im = 0.0;
        wp[B].im = 0.0;
        let mut t = self.ifft.make_output_vec();
        self.ifft.process(&mut wp, &mut t).unwrap();
        for v in t.iter_mut() {
            *v /= N as f32;
        }
        t[B..].iter_mut().for_each(|v| *v = 0.0);
        let mut wf = self.fft.make_output_vec();
        self.fft.process(&mut t, &mut wf).unwrap();
        self.w[p] = wf;
    }
}
