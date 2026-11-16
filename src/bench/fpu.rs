//! Floating-point benchmarks.
//!
//! Both are hand-written rather than taken from a library, so the work is
//! fixed and visible: an FFT's operation count is a known function of its
//! size, which means the result can be reported in FLOP/s rather than as an
//! opaque score.

use std::hint::black_box;
use std::time::Instant;

use crate::bench::{Benchmark, Kind, Measurement};

/// Radix-2 Cooley-Tukey FFT.
pub struct Fft;

/// Transform size. A power of two, as radix-2 requires.
const FFT_POINTS: usize = 1 << 16;
/// Transforms per timed run.
const FFT_RUNS: usize = 50;

impl Benchmark for Fft {
    fn name(&self) -> &'static str {
        "FFT (radix-2)"
    }

    fn kind(&self) -> Kind {
        Kind::Fpu
    }

    fn run(&self) -> Measurement {
        let mut real = vec![0.0f64; FFT_POINTS];
        let mut imag = vec![0.0f64; FFT_POINTS];

        // A warm-up transform, then the timed runs.
        fill(&mut real, &mut imag);
        fft(&mut real, &mut imag);

        let started = Instant::now();
        for _ in 0..FFT_RUNS {
            fill(&mut real, &mut imag);
            fft(black_box(&mut real), black_box(&mut imag));
        }
        let elapsed = started.elapsed();
        black_box((&real, &imag));

        // The conventional count for a complex radix-2 FFT is 5·N·log2(N)
        // floating-point operations per transform.
        let points = FFT_POINTS as f64;
        let flops = 5.0 * points * points.log2() * FFT_RUNS as f64;
        Measurement::throughput(flops / elapsed.as_secs_f64() / 1e6, "MFLOP/s").with_note(format!(
            "{FFT_POINTS}-point complex, {FFT_RUNS} transforms, single thread"
        ))
    }
}

/// A deterministic input signal.
fn fill(real: &mut [f64], imag: &mut [f64]) {
    for (index, (re, im)) in real.iter_mut().zip(imag.iter_mut()).enumerate() {
        let t = index as f64;
        *re = (t * 0.001).sin() + 0.5 * (t * 0.01).cos();
        *im = 0.0;
    }
}

/// In-place iterative radix-2 FFT.
fn fft(real: &mut [f64], imag: &mut [f64]) {
    let n = real.len();
    debug_assert!(n.is_power_of_two(), "radix-2 needs a power-of-two length");

    // Bit-reversal permutation.
    let mut target = 0usize;
    for index in 1..n {
        let mut bit = n >> 1;
        while target & bit != 0 {
            target ^= bit;
            bit >>= 1;
        }
        target |= bit;
        if index < target {
            real.swap(index, target);
            imag.swap(index, target);
        }
    }

    // Butterflies, doubling the span each stage.
    let mut span = 1;
    while span < n {
        // Twiddle step for this stage.
        let theta = -std::f64::consts::PI / span as f64;
        let (step_cos, step_sin) = (theta.cos(), theta.sin());

        for start in (0..n).step_by(span * 2) {
            // Walk the twiddle factor by repeated rotation rather than
            // calling sin/cos per butterfly, which would measure libm.
            let (mut w_re, mut w_im) = (1.0f64, 0.0f64);
            for offset in 0..span {
                let (a, b) = (start + offset, start + offset + span);
                let (re, im) = (real[b], imag[b]);
                let t_re = re * w_re - im * w_im;
                let t_im = re * w_im + im * w_re;

                real[b] = real[a] - t_re;
                imag[b] = imag[a] - t_im;
                real[a] += t_re;
                imag[a] += t_im;

                let next_re = w_re * step_cos - w_im * step_sin;
                w_im = w_re * step_sin + w_im * step_cos;
                w_re = next_re;
            }
        }
        span *= 2;
    }
}

/// A small sphere raytracer: transcendental-heavy floating point with
/// unpredictable branches.
pub struct Raytrace;

const WIDTH: usize = 640;
const HEIGHT: usize = 360;

impl Benchmark for Raytrace {
    fn name(&self) -> &'static str {
        "Raytracing"
    }

    fn kind(&self) -> Kind {
        Kind::Fpu
    }

    fn run(&self) -> Measurement {
        // Warm up on a few rows.
        black_box(render(WIDTH, 8));

        let started = Instant::now();
        let image = render(black_box(WIDTH), black_box(HEIGHT));
        let elapsed = started.elapsed();
        black_box(&image);

        let pixels = (WIDTH * HEIGHT) as f64;
        Measurement::throughput(pixels / elapsed.as_secs_f64() / 1e3, "k pixels/s").with_note(
            format!("{WIDTH}×{HEIGHT}, {SAMPLES} samples per pixel, single thread"),
        )
    }
}

/// Rays per pixel. Enough to make the run non-trivial without taking minutes.
const SAMPLES: usize = 24;

/// Render the scene, returning the accumulated brightness of every pixel.
fn render(width: usize, height: usize) -> Vec<f64> {
    const SPHERES: [([f64; 3], f64); 4] = [
        ([0.0, 0.0, -3.0], 1.0),
        ([1.6, 0.4, -4.0], 0.8),
        ([-1.7, -0.3, -3.5], 0.7),
        ([0.0, -100.5, -4.0], 99.0),
    ];

    let mut image = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let mut brightness = 0.0;
            for sample in 0..SAMPLES {
                // A deterministic jitter inside the pixel, so there is no RNG
                // in the timed loop.
                let jitter = sample as f64 / SAMPLES as f64;
                let u = (x as f64 + jitter) / width as f64 * 2.0 - 1.0;
                let v = (y as f64 + jitter) / height as f64 * 2.0 - 1.0;

                let direction = normalise([u * 1.78, -v, -1.0]);
                brightness += trace(&SPHERES, [0.0, 0.0, 0.0], direction);
            }
            image.push(brightness / SAMPLES as f64);
        }
    }
    image
}

/// Trace one ray and return the lit brightness at its first hit.
fn trace(spheres: &[([f64; 3], f64)], origin: [f64; 3], direction: [f64; 3]) -> f64 {
    let light = normalise([1.0, 1.0, 0.5]);
    let mut nearest = f64::INFINITY;
    let mut normal = [0.0; 3];

    for (centre, radius) in spheres {
        // Ray-sphere intersection: solve |o + td − c|² = r².
        let oc = sub(origin, *centre);
        let b = 2.0 * dot(oc, direction);
        let c = dot(oc, oc) - radius * radius;
        let discriminant = b * b - 4.0 * c;
        if discriminant < 0.0 {
            continue;
        }
        let t = (-b - discriminant.sqrt()) / 2.0;
        if t > 0.001 && t < nearest {
            nearest = t;
            let hit = add(origin, scale(direction, t));
            normal = normalise(sub(hit, *centre));
        }
    }

    if nearest.is_infinite() {
        // Sky: a vertical gradient.
        return 0.5 * (direction[1] + 1.0);
    }
    dot(normal, light).max(0.0)
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalise(a: [f64; 3]) -> [f64; 3] {
    let length = dot(a, a).sqrt();
    if length == 0.0 {
        a
    } else {
        scale(a, 1.0 / length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Naive O(n²) DFT, to check the fast transform against.
    fn dft(real: &[f64], imag: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let n = real.len();
        let mut out_re = vec![0.0; n];
        let mut out_im = vec![0.0; n];
        for k in 0..n {
            for t in 0..n {
                let angle = -2.0 * std::f64::consts::PI * (k * t) as f64 / n as f64;
                out_re[k] += real[t] * angle.cos() - imag[t] * angle.sin();
                out_im[k] += real[t] * angle.sin() + imag[t] * angle.cos();
            }
        }
        (out_re, out_im)
    }

    #[test]
    fn fft_agrees_with_a_naive_dft() {
        // The whole MFLOP/s figure is a lie if the transform is wrong, and a
        // wrong FFT is easy to write and hard to notice.
        let n = 64;
        let mut real: Vec<f64> = (0..n).map(|i| (i as f64 * 0.3).sin()).collect();
        let mut imag = vec![0.0; n];
        let (want_re, want_im) = dft(&real, &imag);

        fft(&mut real, &mut imag);

        for k in 0..n {
            assert!(
                (real[k] - want_re[k]).abs() < 1e-9,
                "bin {k} real: {} vs {}",
                real[k],
                want_re[k]
            );
            assert!(
                (imag[k] - want_im[k]).abs() < 1e-9,
                "bin {k} imag: {} vs {}",
                imag[k],
                want_im[k]
            );
        }
    }

    #[test]
    fn fft_of_a_constant_is_a_single_spike() {
        // DC input must put all its energy in bin 0 and nothing elsewhere.
        let n = 32;
        let mut real = vec![1.0; n];
        let mut imag = vec![0.0; n];
        fft(&mut real, &mut imag);

        assert!((real[0] - n as f64).abs() < 1e-9, "bin 0 = {}", real[0]);
        for k in 1..n {
            assert!(real[k].abs() < 1e-9, "bin {k} should be empty");
            assert!(imag[k].abs() < 1e-9, "bin {k} should be empty");
        }
    }

    #[test]
    fn raytracer_produces_a_plausible_image() {
        let image = render(32, 18);
        assert_eq!(image.len(), 32 * 18);

        // Brightness is a lit dot product or a sky gradient: always in range,
        // and the image must not be uniformly blank.
        assert!(
            image.iter().all(|b| (0.0..=1.5).contains(b)),
            "out of range"
        );
        let min = image.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = image.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(max - min > 0.1, "image is flat: {min} to {max}");
    }

    #[test]
    fn vector_helpers_are_correct() {
        assert_eq!(dot([1.0, 2.0, 3.0], [4.0, 5.0, 6.0]), 32.0);
        assert_eq!(sub([3.0, 3.0, 3.0], [1.0, 2.0, 3.0]), [2.0, 1.0, 0.0]);

        let unit = normalise([3.0, 4.0, 0.0]);
        assert!((dot(unit, unit) - 1.0).abs() < 1e-12, "not a unit vector");
        // A zero vector has no direction and must not produce NaNs.
        assert_eq!(normalise([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0]);
    }
}
