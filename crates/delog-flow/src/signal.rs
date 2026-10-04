use serde::{Deserialize, Serialize};

pub const MAX_ORDER: u32 = 8;
const GAP_FACTOR: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalFilterKind {
    Lowpass,
    Highpass,
    Bandpass,
    Notch,
    MovingAverage,
    Median,
    SavitzkyGolay,
}

impl SignalFilterKind {
    pub const ALL: [Self; 7] = [
        Self::Lowpass,
        Self::Highpass,
        Self::Bandpass,
        Self::Notch,
        Self::MovingAverage,
        Self::Median,
        Self::SavitzkyGolay,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Lowpass => "Lowpass",
            Self::Highpass => "Highpass",
            Self::Bandpass => "Bandpass",
            Self::Notch => "Notch",
            Self::MovingAverage => "Moving Average",
            Self::Median => "Median",
            Self::SavitzkyGolay => "Savitzky-Golay",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "filter", rename_all = "snake_case")]
pub enum SignalFilter {
    Lowpass {
        cutoff_hz: f64,
        order: u32,
        zero_phase: bool,
    },
    Highpass {
        cutoff_hz: f64,
        order: u32,
        zero_phase: bool,
    },
    Bandpass {
        low_hz: f64,
        high_hz: f64,
        order: u32,
        zero_phase: bool,
    },
    Notch {
        center_hz: f64,
        bandwidth_hz: f64,
        zero_phase: bool,
    },
    MovingAverage {
        window_s: f64,
    },
    Median {
        window_s: f64,
    },
    SavitzkyGolay {
        window: u32,
        poly_order: u32,
    },
}

impl SignalFilter {
    pub fn new(kind: SignalFilterKind) -> Self {
        match kind {
            SignalFilterKind::Lowpass => Self::Lowpass {
                cutoff_hz: 10.0,
                order: 2,
                zero_phase: true,
            },
            SignalFilterKind::Highpass => Self::Highpass {
                cutoff_hz: 1.0,
                order: 2,
                zero_phase: true,
            },
            SignalFilterKind::Bandpass => Self::Bandpass {
                low_hz: 1.0,
                high_hz: 10.0,
                order: 2,
                zero_phase: true,
            },
            SignalFilterKind::Notch => Self::Notch {
                center_hz: 50.0,
                bandwidth_hz: 10.0,
                zero_phase: true,
            },
            SignalFilterKind::MovingAverage => Self::MovingAverage { window_s: 0.5 },
            SignalFilterKind::Median => Self::Median { window_s: 0.5 },
            SignalFilterKind::SavitzkyGolay => Self::SavitzkyGolay {
                window: 11,
                poly_order: 2,
            },
        }
    }

    pub fn kind(&self) -> SignalFilterKind {
        match self {
            Self::Lowpass { .. } => SignalFilterKind::Lowpass,
            Self::Highpass { .. } => SignalFilterKind::Highpass,
            Self::Bandpass { .. } => SignalFilterKind::Bandpass,
            Self::Notch { .. } => SignalFilterKind::Notch,
            Self::MovingAverage { .. } => SignalFilterKind::MovingAverage,
            Self::Median { .. } => SignalFilterKind::Median,
            Self::SavitzkyGolay { .. } => SignalFilterKind::SavitzkyGolay,
        }
    }

    pub fn label(&self) -> &'static str {
        self.kind().label()
    }

    pub fn summary(&self) -> String {
        let phase = |zero_phase: bool| if zero_phase { "zero-phase" } else { "causal" };
        match self {
            Self::Lowpass {
                cutoff_hz,
                order,
                zero_phase,
            }
            | Self::Highpass {
                cutoff_hz,
                order,
                zero_phase,
            } => format!("{cutoff_hz} Hz, order {order}, {}", phase(*zero_phase)),
            Self::Bandpass {
                low_hz,
                high_hz,
                order,
                zero_phase,
            } => format!(
                "{low_hz}-{high_hz} Hz, order {order}, {}",
                phase(*zero_phase)
            ),
            Self::Notch {
                center_hz,
                bandwidth_hz,
                zero_phase,
            } => format!(
                "{center_hz} Hz, {bandwidth_hz} Hz wide, {}",
                phase(*zero_phase)
            ),
            Self::MovingAverage { window_s } | Self::Median { window_s } => {
                format!("{window_s} s window")
            }
            Self::SavitzkyGolay { window, poly_order } => {
                format!("{window} samples, order {poly_order}")
            }
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let positive = |value: f64, message: &str| {
            if value.is_finite() && value > 0.0 {
                Ok(())
            } else {
                Err(message.to_owned())
            }
        };
        let order_in_range = |order: u32| {
            if (1..=MAX_ORDER).contains(&order) {
                Ok(())
            } else {
                Err(format!("order must be between 1 and {MAX_ORDER}"))
            }
        };
        match self {
            Self::Lowpass {
                cutoff_hz, order, ..
            }
            | Self::Highpass {
                cutoff_hz, order, ..
            } => {
                positive(*cutoff_hz, "cutoff must be positive")?;
                order_in_range(*order)
            }
            Self::Bandpass {
                low_hz,
                high_hz,
                order,
                ..
            } => {
                positive(*low_hz, "low cutoff must be positive")?;
                positive(*high_hz, "high cutoff must be positive")?;
                if low_hz >= high_hz {
                    return Err("low cutoff must be below high cutoff".to_owned());
                }
                order_in_range(*order)
            }
            Self::Notch {
                center_hz,
                bandwidth_hz,
                ..
            } => {
                positive(*center_hz, "center frequency must be positive")?;
                positive(*bandwidth_hz, "bandwidth must be positive")
            }
            Self::MovingAverage { window_s } | Self::Median { window_s } => {
                positive(*window_s, "window must be positive")
            }
            Self::SavitzkyGolay { window, poly_order } => {
                if *window < 3 || window % 2 == 0 {
                    return Err("window must be an odd number of at least 3 samples".to_owned());
                }
                if poly_order >= window {
                    return Err("polynomial order must be less than the window".to_owned());
                }
                Ok(())
            }
        }
    }

    pub fn apply(&self, t: &[i64], v: &[f64]) -> Result<(Vec<f64>, Vec<String>), String> {
        self.validate()?;
        let mut output = v.to_vec();
        let Some(step_us) = median_step_us(t) else {
            return Ok((output, Vec::new()));
        };
        let segments = segments(t, v, step_us);
        let mut messages = Vec::new();
        match self {
            Self::Lowpass { .. }
            | Self::Highpass { .. }
            | Self::Bandpass { .. }
            | Self::Notch { .. } => {
                let (sections, zero_phase) = self.design(1e6 / step_us)?;
                for range in segments {
                    let segment = &mut output[range];
                    if zero_phase {
                        filtfilt(&sections, segment);
                    } else {
                        for section in &sections {
                            section.run(segment);
                        }
                    }
                }
            }
            Self::MovingAverage { window_s } => {
                for range in segments {
                    moving_average(
                        &t[range.clone()],
                        &v[range.clone()],
                        &mut output[range],
                        window_s * 0.5e6,
                    );
                }
            }
            Self::Median { window_s } => {
                for range in segments {
                    moving_median(
                        &t[range.clone()],
                        &v[range.clone()],
                        &mut output[range],
                        window_s * 0.5e6,
                    );
                }
            }
            Self::SavitzkyGolay { window, poly_order } => {
                if unevenly_spaced(t, step_us) {
                    messages.push(
                        "timestamps are unevenly spaced; Savitzky-Golay assumes a constant sample rate"
                            .to_owned(),
                    );
                }
                let window = *window as usize;
                let kernel = SavGol::new(window, *poly_order as usize);
                for range in segments {
                    if range.len() >= window {
                        kernel.run(&v[range.clone()], &mut output[range]);
                    }
                }
            }
        }
        Ok((output, messages))
    }

    fn design(&self, rate_hz: f64) -> Result<(Vec<Biquad>, bool), String> {
        let nyquist = rate_hz / 2.0;
        let below_nyquist = |hz: f64, name: &str| {
            if hz < nyquist {
                Ok(())
            } else {
                Err(format!(
                    "{name} {hz} Hz must be below the {nyquist:.4} Hz Nyquist limit of this signal"
                ))
            }
        };
        Ok(match *self {
            Self::Lowpass {
                cutoff_hz,
                order,
                zero_phase,
            } => {
                below_nyquist(cutoff_hz, "cutoff")?;
                (
                    butterworth(Pass::Low, cutoff_hz / rate_hz, order),
                    zero_phase,
                )
            }
            Self::Highpass {
                cutoff_hz,
                order,
                zero_phase,
            } => {
                below_nyquist(cutoff_hz, "cutoff")?;
                (
                    butterworth(Pass::High, cutoff_hz / rate_hz, order),
                    zero_phase,
                )
            }
            Self::Bandpass {
                low_hz,
                high_hz,
                order,
                zero_phase,
            } => {
                below_nyquist(high_hz, "high cutoff")?;
                let mut sections = butterworth(Pass::High, low_hz / rate_hz, order);
                sections.extend(butterworth(Pass::Low, high_hz / rate_hz, order));
                (sections, zero_phase)
            }
            Self::Notch {
                center_hz,
                bandwidth_hz,
                zero_phase,
            } => {
                below_nyquist(center_hz, "center frequency")?;
                (
                    vec![Biquad::notch(center_hz / rate_hz, center_hz / bandwidth_hz)],
                    zero_phase,
                )
            }
            _ => unreachable!("only frequency filters have a design"),
        })
    }
}

fn median_step_us(t: &[i64]) -> Option<f64> {
    let mut steps: Vec<i64> = t
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .filter(|&step| step > 0)
        .collect();
    if steps.is_empty() {
        return None;
    }
    let middle = steps.len() / 2;
    let (_, &mut step, _) = steps.select_nth_unstable(middle);
    Some(step as f64)
}

fn segments(t: &[i64], v: &[f64], step_us: f64) -> Vec<std::ops::Range<usize>> {
    let gap_us = step_us * GAP_FACTOR;
    let mut ranges = Vec::new();
    let mut start: Option<usize> = None;
    for index in 0..v.len() {
        if !v[index].is_finite() {
            if let Some(first) = start.take() {
                ranges.push(first..index);
            }
            continue;
        }
        if let Some(first) = start
            && (t[index] - t[index - 1]) as f64 > gap_us
        {
            ranges.push(first..index);
            start = Some(index);
        }
        start.get_or_insert(index);
    }
    if let Some(first) = start {
        ranges.push(first..v.len());
    }
    ranges
}

fn unevenly_spaced(t: &[i64], step_us: f64) -> bool {
    let tolerance = step_us * 0.25;
    let gap_us = step_us * GAP_FACTOR;
    let steps: Vec<f64> = t
        .windows(2)
        .map(|pair| (pair[1] - pair[0]) as f64)
        .filter(|&step| step <= gap_us)
        .collect();
    let uneven = steps
        .iter()
        .filter(|&&step| (step - step_us).abs() > tolerance)
        .count();
    uneven * 100 > steps.len()
}

#[derive(Clone, Copy)]
enum Pass {
    Low,
    High,
}

#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn first_order(pass: Pass, cycles_per_sample: f64) -> Self {
        let k = (std::f64::consts::PI * cycles_per_sample).tan();
        let a1 = (k - 1.0) / (k + 1.0);
        let (b0, b1) = match pass {
            Pass::Low => (k / (1.0 + k), k / (1.0 + k)),
            Pass::High => (1.0 / (1.0 + k), -1.0 / (1.0 + k)),
        };
        Self {
            b0,
            b1,
            b2: 0.0,
            a1,
            a2: 0.0,
        }
    }

    fn second_order(pass: Pass, cycles_per_sample: f64, q: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * cycles_per_sample;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1) = match pass {
            Pass::Low => ((1.0 - cos) / 2.0, 1.0 - cos),
            Pass::High => ((1.0 + cos) / 2.0, -(1.0 + cos)),
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn notch(cycles_per_sample: f64, q: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * cycles_per_sample;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: 1.0 / a0,
            b1: -2.0 * cos / a0,
            b2: 1.0 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn run(&self, values: &mut [f64]) {
        let Some(&first) = values.first() else {
            return;
        };
        let gain = (self.b0 + self.b1 + self.b2) / (1.0 + self.a1 + self.a2);
        let mut s2 = (self.b2 - self.a2 * gain) * first;
        let mut s1 = (self.b1 - self.a1 * gain) * first + s2;
        for value in values {
            let x = *value;
            let y = self.b0 * x + s1;
            s1 = self.b1 * x - self.a1 * y + s2;
            s2 = self.b2 * x - self.a2 * y;
            *value = y;
        }
    }
}

fn butterworth(pass: Pass, cycles_per_sample: f64, order: u32) -> Vec<Biquad> {
    let mut sections: Vec<Biquad> = (0..order / 2)
        .map(|k| {
            let angle = std::f64::consts::PI * f64::from(2 * k + 1) / f64::from(2 * order);
            Biquad::second_order(pass, cycles_per_sample, 1.0 / (2.0 * angle.sin()))
        })
        .collect();
    if order % 2 == 1 {
        sections.push(Biquad::first_order(pass, cycles_per_sample));
    }
    sections
}

fn filtfilt(sections: &[Biquad], values: &mut [f64]) {
    let n = values.len();
    if n < 2 {
        return;
    }
    let pad = (3 * (2 * sections.len() + 1)).min(n - 1);
    let (first, last) = (values[0], values[n - 1]);
    let mut extended = Vec::with_capacity(n + 2 * pad);
    extended.extend((1..=pad).rev().map(|i| 2.0 * first - values[i]));
    extended.extend_from_slice(values);
    extended.extend((1..=pad).map(|i| 2.0 * last - values[n - 1 - i]));
    for section in sections {
        section.run(&mut extended);
    }
    extended.reverse();
    for section in sections {
        section.run(&mut extended);
    }
    extended.reverse();
    values.copy_from_slice(&extended[pad..pad + n]);
}

fn moving_average(t: &[i64], v: &[f64], output: &mut [f64], half_us: f64) {
    let (mut lo, mut hi, mut sum) = (0, 0, 0.0);
    for i in 0..v.len() {
        while hi < v.len() && (t[hi] - t[i]) as f64 <= half_us {
            sum += v[hi];
            hi += 1;
        }
        while (t[i] - t[lo]) as f64 > half_us {
            sum -= v[lo];
            lo += 1;
        }
        output[i] = sum / (hi - lo) as f64;
    }
}

fn moving_median(t: &[i64], v: &[f64], output: &mut [f64], half_us: f64) {
    let (mut lo, mut hi) = (0, 0);
    let mut sorted: Vec<f64> = Vec::new();
    for i in 0..v.len() {
        while hi < v.len() && (t[hi] - t[i]) as f64 <= half_us {
            let at = sorted.partition_point(|&x| x < v[hi]);
            sorted.insert(at, v[hi]);
            hi += 1;
        }
        while (t[i] - t[lo]) as f64 > half_us {
            let at = sorted.partition_point(|&x| x < v[lo]);
            sorted.remove(at);
            lo += 1;
        }
        let middle = sorted.len() / 2;
        output[i] = if sorted.len() % 2 == 1 {
            sorted[middle]
        } else {
            (sorted[middle - 1] + sorted[middle]) / 2.0
        };
    }
}

struct SavGol {
    window: usize,
    weights: Vec<Vec<f64>>,
}

impl SavGol {
    fn new(window: usize, poly_order: usize) -> Self {
        let half = window / 2;
        let weights = (0..window)
            .map(|position| savgol_weights(window, poly_order, position as f64 - half as f64))
            .collect();
        Self { window, weights }
    }

    fn run(&self, v: &[f64], output: &mut [f64]) {
        let half = self.window / 2;
        let n = v.len();
        let dot = |weights: &[f64], start: usize| {
            weights
                .iter()
                .zip(&v[start..start + self.window])
                .map(|(w, x)| w * x)
                .sum::<f64>()
        };
        for (i, value) in output.iter_mut().enumerate() {
            *value = if i < half {
                dot(&self.weights[i], 0)
            } else if i + half >= n {
                dot(&self.weights[i + self.window - n], n - self.window)
            } else {
                dot(&self.weights[half], i - half)
            };
        }
    }
}

fn savgol_weights(window: usize, poly_order: usize, at: f64) -> Vec<f64> {
    let half = (window / 2) as f64;
    let terms = poly_order + 1;
    let powers = |x: f64| -> Vec<f64> {
        let x = x / half;
        (0..terms).map(|k| x.powi(k as i32)).collect()
    };
    let rows: Vec<Vec<f64>> = (0..window).map(|j| powers(j as f64 - half)).collect();
    let mut normal = vec![vec![0.0; terms + 1]; terms];
    for (r, row) in normal.iter_mut().enumerate() {
        for c in 0..terms {
            row[c] = rows.iter().map(|p| p[r] * p[c]).sum();
        }
    }
    for (row, target) in normal.iter_mut().zip(powers(at)) {
        row[terms] = target;
    }
    let solution = solve(normal);
    rows.iter()
        .map(|p| p.iter().zip(&solution).map(|(a, b)| a * b).sum())
        .collect()
}

fn solve(mut augmented: Vec<Vec<f64>>) -> Vec<f64> {
    let n = augmented.len();
    for col in 0..n {
        let pivot = (col..n)
            .max_by(|&a, &b| augmented[a][col].abs().total_cmp(&augmented[b][col].abs()))
            .unwrap_or(col);
        augmented.swap(col, pivot);
        let (upper, lower) = augmented.split_at_mut(col + 1);
        let pivot_row = &upper[col];
        for row in lower {
            let factor = row[col] / pivot_row[col];
            for (target, source) in row[col..].iter_mut().zip(&pivot_row[col..]) {
                *target -= factor * source;
            }
        }
    }
    let mut solution = vec![0.0; n];
    for row in (0..n).rev() {
        let tail: f64 = (row + 1..n).map(|k| augmented[row][k] * solution[k]).sum();
        solution[row] = (augmented[row][n] - tail) / augmented[row][row];
    }
    solution
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn uniform(rate_hz: f64, seconds: f64) -> Vec<i64> {
        let n = (rate_hz * seconds) as usize;
        (0..n).map(|i| (i as f64 * 1e6 / rate_hz) as i64).collect()
    }

    fn sine(t: &[i64], freq_hz: f64) -> Vec<f64> {
        t.iter()
            .map(|&us| (2.0 * PI * freq_hz * us as f64 * 1e-6).sin())
            .collect()
    }

    fn rms_middle(v: &[f64]) -> f64 {
        let middle = &v[v.len() / 4..v.len() * 3 / 4];
        (middle.iter().map(|x| x * x).sum::<f64>() / middle.len() as f64).sqrt()
    }

    fn gain(filter: &SignalFilter, freq_hz: f64) -> f64 {
        let t = uniform(1000.0, 4.0);
        let input = sine(&t, freq_hz);
        let (output, _) = filter.apply(&t, &input).unwrap();
        rms_middle(&output) / rms_middle(&input)
    }

    fn lowpass(cutoff_hz: f64, order: u32, zero_phase: bool) -> SignalFilter {
        SignalFilter::Lowpass {
            cutoff_hz,
            order,
            zero_phase,
        }
    }

    #[test]
    fn every_kind_has_valid_defaults_and_a_label() {
        for kind in SignalFilterKind::ALL {
            let filter = SignalFilter::new(kind);
            assert_eq!(filter.kind(), kind);
            assert_eq!(filter.validate(), Ok(()));
            assert!(!filter.label().is_empty());
            assert!(!filter.summary().is_empty());
        }
    }

    #[test]
    fn lowpass_passes_low_and_attenuates_high_frequencies() {
        for order in 1..=MAX_ORDER {
            for zero_phase in [true, false] {
                let filter = lowpass(10.0, order, zero_phase);
                assert!(gain(&filter, 1.0) > 0.95, "order {order}");
                assert!(gain(&filter, 80.0) < 0.2, "order {order}");
            }
        }
    }

    #[test]
    fn butterworth_cutoff_is_minus_three_db_per_pass() {
        for order in 1..=MAX_ORDER {
            let single = gain(&lowpass(20.0, order, false), 20.0);
            assert!(
                (single - 0.5f64.sqrt()).abs() < 0.03,
                "order {order}: {single}"
            );
            let double = gain(&lowpass(20.0, order, true), 20.0);
            assert!((double - 0.5).abs() < 0.03, "order {order}: {double}");
        }
    }

    #[test]
    fn highpass_removes_dc_and_keeps_high_frequencies() {
        let filter = SignalFilter::Highpass {
            cutoff_hz: 1.0,
            order: 2,
            zero_phase: true,
        };
        let t = uniform(1000.0, 4.0);
        let input: Vec<f64> = sine(&t, 50.0).iter().map(|x| x + 7.0).collect();
        let (output, _) = filter.apply(&t, &input).unwrap();
        let middle = &output[output.len() / 4..output.len() * 3 / 4];
        let mean = middle.iter().sum::<f64>() / middle.len() as f64;
        assert!(mean.abs() < 0.01, "{mean}");
        assert!((rms_middle(&output) - 0.5f64.sqrt()).abs() < 0.02);
    }

    #[test]
    fn bandpass_keeps_the_band_and_rejects_both_sides() {
        let filter = SignalFilter::Bandpass {
            low_hz: 5.0,
            high_hz: 20.0,
            order: 4,
            zero_phase: true,
        };
        assert!(gain(&filter, 10.0) > 0.9);
        assert!(gain(&filter, 0.5) < 0.05);
        assert!(gain(&filter, 150.0) < 0.05);
    }

    #[test]
    fn notch_removes_the_center_frequency_only() {
        let filter = SignalFilter::Notch {
            center_hz: 50.0,
            bandwidth_hz: 10.0,
            zero_phase: true,
        };
        assert!(gain(&filter, 50.0) < 0.05);
        assert!(gain(&filter, 5.0) > 0.95);
        assert!(gain(&filter, 200.0) > 0.95);
    }

    #[test]
    fn zero_phase_output_has_no_lag_and_causal_output_lags() {
        let t = uniform(1000.0, 4.0);
        let input = sine(&t, 2.0);
        let lag = |zero_phase| {
            let (output, _) = lowpass(10.0, 4, zero_phase).apply(&t, &input).unwrap();
            let range = t.len() / 4..t.len() * 3 / 4;
            (0..50)
                .min_by(|&a, &b| {
                    let error = |shift: usize| {
                        range
                            .clone()
                            .map(|i| (output[i + shift] - input[i]).powi(2))
                            .sum::<f64>()
                    };
                    error(a).total_cmp(&error(b))
                })
                .unwrap()
        };
        assert_eq!(lag(true), 0);
        assert!(lag(false) > 10);
    }

    #[test]
    fn a_constant_signal_has_no_startup_transient() {
        let t = uniform(100.0, 2.0);
        let input = vec![3.5; t.len()];
        for zero_phase in [true, false] {
            let (output, _) = lowpass(5.0, 4, zero_phase).apply(&t, &input).unwrap();
            assert!(output.iter().all(|y| (y - 3.5).abs() < 1e-9), "{output:?}");
        }
    }

    #[test]
    fn cutoff_at_or_above_nyquist_is_rejected() {
        let t = uniform(100.0, 1.0);
        let input = sine(&t, 1.0);
        let error = lowpass(50.0, 2, true).apply(&t, &input).unwrap_err();
        assert!(error.contains("Nyquist"), "{error}");
        let band = SignalFilter::Bandpass {
            low_hz: 10.0,
            high_hz: 60.0,
            order: 2,
            zero_phase: true,
        };
        assert!(band.apply(&t, &input).unwrap_err().contains("Nyquist"));
    }

    #[test]
    fn nan_samples_pass_through_and_split_segments() {
        let t = uniform(100.0, 2.0);
        let mut input = vec![1.0; t.len()];
        for value in &mut input[100..] {
            *value = 5.0;
        }
        input[100] = f64::NAN;
        for filter in SignalFilterKind::ALL.map(SignalFilter::new) {
            let filter = match filter {
                SignalFilter::Highpass { .. } | SignalFilter::Bandpass { .. } => continue,
                SignalFilter::Notch { zero_phase, .. } => SignalFilter::Notch {
                    center_hz: 20.0,
                    bandwidth_hz: 5.0,
                    zero_phase,
                },
                other => other,
            };
            let (output, _) = filter.apply(&t, &input).unwrap();
            assert!(output[100].is_nan(), "{filter:?}");
            assert!(
                output[..100].iter().all(|y| (y - 1.0).abs() < 1e-9),
                "{filter:?}"
            );
            assert!(
                output[101..].iter().all(|y| (y - 5.0).abs() < 1e-9),
                "{filter:?}"
            );
        }
    }

    #[test]
    fn a_long_time_gap_splits_segments() {
        let mut t = uniform(100.0, 1.0);
        let jump = t.last().unwrap() + 1_000_000;
        t.extend((0..100).map(|i| jump + i * 10_000));
        let input: Vec<f64> = (0..t.len())
            .map(|i| if i < 100 { 0.0 } else { 10.0 })
            .collect();
        let (output, _) = lowpass(2.0, 2, true).apply(&t, &input).unwrap();
        assert!(output[..100].iter().all(|y| y.abs() < 1e-9));
        assert!(output[100..].iter().all(|y| (y - 10.0).abs() < 1e-9));
    }

    #[test]
    fn moving_average_uses_a_centered_time_window() {
        let t: Vec<i64> = (0..5).map(|i| i * 100_000).collect();
        let input = [0.0, 0.0, 3.0, 0.0, 0.0];
        let (output, _) = SignalFilter::MovingAverage { window_s: 0.2 }
            .apply(&t, &input)
            .unwrap();
        assert_eq!(output, vec![0.0, 1.0, 1.0, 1.0, 0.0]);
    }

    #[test]
    fn moving_average_window_follows_time_not_sample_count() {
        let t = [0, 10_000, 20_000, 30_000, 200_000];
        let input = [1.0, 2.0, 3.0, 4.0, 100.0];
        let (output, _) = SignalFilter::MovingAverage { window_s: 0.05 }
            .apply(&t, &input)
            .unwrap();
        assert_eq!(output[..4], [2.0, 2.5, 2.5, 3.0]);
    }

    #[test]
    fn median_removes_an_isolated_spike() {
        let t = uniform(100.0, 1.0);
        let mut input = vec![2.0; t.len()];
        input[50] = 1000.0;
        let (output, _) = SignalFilter::Median { window_s: 0.05 }
            .apply(&t, &input)
            .unwrap();
        assert!(output.iter().all(|&y| y == 2.0));
    }

    #[test]
    fn median_of_an_even_window_averages_the_middle_pair() {
        let t = [0, 10_000, 20_000, 30_000];
        let input = [1.0, 2.0, 10.0, 20.0];
        let (output, _) = SignalFilter::Median { window_s: 0.02 }
            .apply(&t, &input)
            .unwrap();
        assert_eq!(output, vec![1.5, 2.0, 10.0, 15.0]);
    }

    #[test]
    fn savitzky_golay_preserves_a_polynomial_of_its_order_including_edges() {
        let t = uniform(100.0, 1.0);
        let input: Vec<f64> = (0..t.len())
            .map(|i| {
                let x = i as f64;
                0.5 * x * x - 3.0 * x + 2.0
            })
            .collect();
        let (output, messages) = SignalFilter::SavitzkyGolay {
            window: 11,
            poly_order: 2,
        }
        .apply(&t, &input)
        .unwrap();
        assert!(messages.is_empty());
        for (y, x) in output.iter().zip(&input) {
            assert!((y - x).abs() < 1e-6, "{y} vs {x}");
        }
    }

    #[test]
    fn savitzky_golay_smooths_noise() {
        let t = uniform(100.0, 2.0);
        let input: Vec<f64> = (0..t.len())
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let (output, _) = SignalFilter::SavitzkyGolay {
            window: 11,
            poly_order: 2,
        }
        .apply(&t, &input)
        .unwrap();
        assert!(rms_middle(&output) < 0.2);
    }

    #[test]
    fn savitzky_golay_warns_about_uneven_spacing() {
        let t: Vec<i64> = (0..100).map(|i| i * 10_000 + (i % 3) * 4_000).collect();
        let input = vec![0.0; t.len()];
        let (_, messages) = SignalFilter::SavitzkyGolay {
            window: 11,
            poly_order: 2,
        }
        .apply(&t, &input)
        .unwrap();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("unevenly spaced"), "{messages:?}");
    }

    #[test]
    fn segments_shorter_than_the_window_pass_through() {
        let t = [0, 10_000, 20_000];
        let input = [1.0, 5.0, 2.0];
        let (output, _) = SignalFilter::SavitzkyGolay {
            window: 11,
            poly_order: 2,
        }
        .apply(&t, &input)
        .unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn empty_and_single_sample_signals_pass_through() {
        for filter in SignalFilterKind::ALL.map(SignalFilter::new) {
            assert_eq!(filter.apply(&[], &[]).unwrap().0, Vec::<f64>::new());
            assert_eq!(filter.apply(&[5], &[2.0]).unwrap().0, vec![2.0]);
        }
    }

    #[test]
    fn invalid_settings_are_rejected_with_styled_messages() {
        let cases = [
            (lowpass(0.0, 2, true), "cutoff must be positive"),
            (lowpass(f64::NAN, 2, true), "cutoff must be positive"),
            (lowpass(10.0, 0, true), "order must be between 1 and 8"),
            (lowpass(10.0, 9, true), "order must be between 1 and 8"),
            (
                SignalFilter::Bandpass {
                    low_hz: 10.0,
                    high_hz: 5.0,
                    order: 2,
                    zero_phase: true,
                },
                "low cutoff must be below high cutoff",
            ),
            (
                SignalFilter::Notch {
                    center_hz: 50.0,
                    bandwidth_hz: 0.0,
                    zero_phase: true,
                },
                "bandwidth must be positive",
            ),
            (
                SignalFilter::MovingAverage { window_s: -1.0 },
                "window must be positive",
            ),
            (
                SignalFilter::Median {
                    window_s: f64::INFINITY,
                },
                "window must be positive",
            ),
            (
                SignalFilter::SavitzkyGolay {
                    window: 10,
                    poly_order: 2,
                },
                "window must be an odd number of at least 3 samples",
            ),
            (
                SignalFilter::SavitzkyGolay {
                    window: 5,
                    poly_order: 5,
                },
                "polynomial order must be less than the window",
            ),
        ];
        for (filter, message) in cases {
            assert_eq!(filter.validate(), Err(message.to_owned()), "{filter:?}");
        }
    }

    #[test]
    fn serializes_with_a_filter_tag() {
        let json = serde_json::to_value(lowpass(10.0, 2, true)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"filter": "lowpass", "cutoff_hz": 10.0, "order": 2, "zero_phase": true})
        );
        for filter in SignalFilterKind::ALL.map(SignalFilter::new) {
            let json = serde_json::to_value(&filter).unwrap();
            assert_eq!(
                serde_json::from_value::<SignalFilter>(json).unwrap(),
                filter
            );
        }
    }
}
