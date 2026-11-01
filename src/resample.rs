//! Streaming windowed-sinc resampler with a slightly adjustable ratio (for clock-drift control).

const RES: usize = 256; // kernel table points per input sample
const ZERO_CROSSINGS: f64 = 12.0;

pub struct Resampler {
    /// Input samples advanced per output sample, before `adjust`.
    base_step: f64,
    adjust: f64,
    half: usize,
    /// Kernel h(|x|) at x = i / RES, i in 0..=half*RES+1.
    table: Vec<f32>,
    buf: Vec<f32>,
    /// Position of the next output sample, in `buf` indices.
    pos: f64,
    passthrough: bool,
}

impl Resampler {
    pub fn new(in_rate: f64, out_rate: f64) -> Self {
        let passthrough = in_rate == out_rate;
        // Cutoff in cycles per input sample: 90% of the lower Nyquist.
        let fc = 0.45 * in_rate.min(out_rate) / in_rate;
        let half = (ZERO_CROSSINGS / (2.0 * fc)).ceil() as usize;
        let table = (0..=half * RES + 1)
            .map(|i| {
                let x = i as f64 / RES as f64;
                if x >= half as f64 {
                    return 0.0;
                }
                let t = 2.0 * fc * x;
                let sinc = if t == 0.0 {
                    1.0
                } else {
                    (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
                };
                // Blackman window over [-half, half].
                let w = 0.42
                    + 0.5 * (std::f64::consts::PI * x / half as f64).cos()
                    + 0.08 * (2.0 * std::f64::consts::PI * x / half as f64).cos();
                (2.0 * fc * sinc * w) as f32
            })
            .collect();
        Self {
            base_step: in_rate / out_rate,
            adjust: 0.0,
            half,
            table,
            buf: vec![0.0; half],
            pos: half as f64,
            passthrough,
        }
    }

    /// Relative ratio tweak: positive values produce slightly fewer output samples.
    pub fn set_adjust(&mut self, adjust: f64) {
        self.adjust = adjust;
        if adjust != 0.0 {
            self.passthrough = false;
        }
    }

    fn kernel(&self, x: f64) -> f32 {
        let p = x.abs() * RES as f64;
        let i = p as usize;
        if i + 1 >= self.table.len() {
            return 0.0;
        }
        let f = (p - i as f64) as f32;
        self.table[i] + (self.table[i + 1] - self.table[i]) * f
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        let step = self.base_step * (1.0 + self.adjust);
        let half = self.half as isize;
        loop {
            let center = self.pos.floor() as isize;
            if center + half >= self.buf.len() as isize {
                break;
            }
            let mut acc = 0f32;
            for k in (center - half + 1)..=(center + half) {
                acc += self.buf[k as usize] * self.kernel(k as f64 - self.pos);
            }
            out.push(acc);
            self.pos += step;
        }
        let keep_from = (self.pos.floor() as isize - half + 1).max(0) as usize;
        let keep_from = keep_from.min(self.buf.len());
        self.buf.drain(..keep_from);
        self.pos -= keep_from as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: f64, freq: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin() as f32)
            .collect()
    }

    fn check(in_rate: f64, out_rate: f64) {
        let input = tone(in_rate, 1000.0, in_rate as usize);
        let mut r = Resampler::new(in_rate, out_rate);
        let mut out = Vec::new();
        for c in input.chunks(97) {
            r.process(c, &mut out);
        }
        let expected = out_rate as usize;
        assert!(
            (out.len() as isize - expected as isize).abs() < 100,
            "{} vs {expected}",
            out.len()
        );
        // Output i is input time i / out_rate: the kernel only looks ahead, it does not shift.
        let delay = 0.0;
        let ideal = |i: usize| {
            (2.0 * std::f64::consts::PI * 1000.0 * (i as f64 - delay) / out_rate).sin() as f32
        };
        let err: f32 = (2000..out.len() - 2000)
            .map(|i| (out[i] - ideal(i)).abs())
            .fold(0.0, f32::max);
        assert!(err < 0.01, "{in_rate}->{out_rate}: max error {err}");
    }

    #[test]
    fn down_and_up() {
        check(48000.0, 16000.0);
        check(16000.0, 48000.0);
        check(44100.0, 16000.0);
        check(16000.0, 44100.0);
    }
}
