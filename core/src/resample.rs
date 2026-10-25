//! Streaming resampler: windowed-sinc interpolation. To 16 kHz for the pitch tracker it low-passes at 7.2 kHz.
pub struct Resampler {
    step: f64,          // input samples per output sample
    fc: f64,            // cutoff, as a fraction of the input rate
    half: usize,        // filter half-width in input samples
    buf: Vec<f32>,      // input not yet fully consumed
    pos: f64,           // next output position within buf
}

impl Resampler {
    pub fn new(in_rate: f64, out_rate: f64) -> Resampler {
        Resampler::with_cutoff(in_rate, out_rate, 7200.0)
    }

    /// `cutoff_hz` must be below half of both rates.
    pub fn with_cutoff(in_rate: f64, out_rate: f64, cutoff_hz: f64) -> Resampler {
        let step = in_rate / out_rate;
        Resampler { step, fc: cutoff_hz / in_rate, half: (16.0 * step.max(1.0)).ceil() as usize, buf: Vec::new(), pos: 0.0 }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buf.extend_from_slice(input);
        let h = self.half as f64;
        while self.pos + h + 1.0 < self.buf.len() as f64 {
            let c = self.pos.floor() as isize;
            let mut acc = 0.0f64;
            for k in (c - self.half as isize + 1)..=(c + self.half as isize) {
                if k < 0 {
                    continue;
                }
                let t = self.pos - k as f64;
                let x = 2.0 * self.fc * t;
                let sinc = if x.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
                let w = 0.5 + 0.5 * (std::f64::consts::PI * t / h).cos();         // Hann window
                acc += self.buf[k as usize] as f64 * 2.0 * self.fc * sinc * w;
            }
            out.push(acc as f32);
            self.pos += self.step;
        }
        // drop what no future output needs
        let keep_from = (self.pos.floor() as isize - self.half as isize).max(0) as usize;
        if keep_from > 0 {
            self.buf.drain(..keep_from);
            self.pos -= keep_from as f64;
        }
    }
}
