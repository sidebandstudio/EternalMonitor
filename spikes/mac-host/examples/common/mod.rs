//! Shared by the VideoToolbox spikes. Throwaway.
#![allow(dead_code)]

pub mod bitstream;
pub mod vt;

use std::time::Duration;

pub const INNER: (u32, u32) = (2670, 1878);
pub const OUTER: (u32, u32) = (2034, 1398);

pub fn parse_size(value: &str) -> (u32, u32) {
    match value {
        "inner" => INNER,
        "outer" => OUTER,
        other => {
            let (w, h) = other.split_once('x').expect("size is inner, outer or WxH");
            (w.parse().unwrap(), h.parse().unwrap())
        }
    }
}

/// `--key value` arguments.
pub struct Args(Vec<String>);

impl Args {
    pub fn from_env() -> Self {
        Self(std::env::args().skip(1).collect())
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        let flag = format!("--{key}");
        self.0
            .iter()
            .position(|a| *a == flag)
            .and_then(|i| self.0.get(i + 1))
            .map(String::as_str)
    }
    pub fn flag(&self, key: &str) -> bool {
        self.0.iter().any(|a| *a == format!("--{key}"))
    }
    pub fn or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.get(key).unwrap_or(default)
    }
    pub fn num<T: std::str::FromStr>(&self, key: &str, default: T) -> T
    where
        T::Err: std::fmt::Debug,
    {
        self.get(key).map_or(default, |v| v.parse().unwrap())
    }
}

/// p-th percentile of microsecond samples, in milliseconds.
pub fn percentile_ms(sorted_us: &[u64], p: f64) -> f64 {
    if sorted_us.is_empty() {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * (sorted_us.len() - 1) as f64).round() as usize;
    sorted_us[rank.min(sorted_us.len() - 1)] as f64 / 1000.0
}

pub struct LatencySummary {
    pub count: usize,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

pub fn summarize(mut samples_us: Vec<u64>) -> LatencySummary {
    samples_us.sort_unstable();
    LatencySummary {
        count: samples_us.len(),
        p50: percentile_ms(&samples_us, 50.0),
        p95: percentile_ms(&samples_us, 95.0),
        p99: percentile_ms(&samples_us, 99.0),
        max: samples_us.last().map_or(f64::NAN, |&m| m as f64 / 1000.0),
    }
}

impl std::fmt::Display for LatencySummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "n={} p50={:.2}ms p95={:.2}ms p99={:.2}ms max={:.2}ms",
            self.count, self.p50, self.p95, self.p99, self.max
        )
    }
}

/// Sleeps until `deadline`, spinning for the last half millisecond so a
/// 120 fps cadence is not at the mercy of timer coalescing.
pub fn sleep_until(deadline: std::time::Instant) {
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return;
        }
        let left = deadline - now;
        if left > Duration::from_micros(700) {
            std::thread::sleep(left - Duration::from_micros(500));
        } else {
            std::hint::spin_loop();
        }
    }
}

/// One-minute load average and the thermal state `pmset` reports.
pub fn machine_state() -> String {
    let load = std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let therm = std::process::Command::new("pmset")
        .args(["-g", "therm"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.contains('='))
                .map(|l| l.split_whitespace().collect::<Vec<_>>().join(""))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    format!("load={load} therm=[{therm}]")
}

/// Fills a luma-like plane with a frame-dependent pattern: a diagonal
/// gradient, a moving bright bar and a noisy block, so every frame differs
/// and the encoder cannot skip.
pub fn fill_luma(plane: &mut [u8], stride: usize, width: usize, height: usize, frame: usize) {
    let mut seed = 0x9e37_79b9_u32.wrapping_mul(frame as u32 + 1);
    let bar = (frame * 37) % width;
    for y in 0..height {
        let row = &mut plane[y * stride..y * stride + width];
        for (x, px) in row.iter_mut().enumerate() {
            let mut v = ((x + y + frame * 4) & 0xff) as u8;
            if x.abs_diff(bar) < 40 {
                v = 235;
            }
            if x < width / 4 && y < height / 4 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                v = (seed >> 24) as u8;
            }
            *px = v;
        }
    }
}

pub fn fill_chroma(
    plane: &mut [u8],
    stride: usize,
    width_bytes: usize,
    height: usize,
    frame: usize,
) {
    for y in 0..height {
        for (x, px) in plane[y * stride..y * stride + width_bytes]
            .iter_mut()
            .enumerate()
        {
            *px = (128 + ((x / 64 + y / 64 + frame) % 32)) as u8;
        }
    }
}

pub fn fill_bgra(plane: &mut [u8], stride: usize, width: usize, height: usize, frame: usize) {
    let mut luma = vec![0u8; width * height];
    fill_luma(&mut luma, width, width, height, frame);
    for y in 0..height {
        for x in 0..width {
            let v = luma[y * width + x];
            let o = y * stride + x * 4;
            plane[o] = v;
            plane[o + 1] = v.wrapping_add((x / 8) as u8);
            plane[o + 2] = v.wrapping_add((y / 8) as u8);
            plane[o + 3] = 255;
        }
    }
}
