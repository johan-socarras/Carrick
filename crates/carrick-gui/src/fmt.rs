//! Numbers as a person reads them.

use std::time::Duration;

/// Sizes in binary units, like the terminal: `9.0 MiB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let digits = if value < 10.0 { 2 } else if value < 100.0 { 1 } else { 0 };
    format!("{value:.digits$} {}", UNITS[unit])
}

/// A rough time left: `42 s`, `8 min`, `1 h 05 min`.
pub fn eta(left: Duration) -> String {
    let s = left.as_secs();
    if s < 60 {
        format!("{} s", s.max(1))
    } else if s < 3600 {
        format!("{} min", s.div_ceil(60))
    } else {
        format!("{} h {:02} min", s / 3600, (s % 3600) / 60)
    }
}

/// `n file` or `n files`.
pub fn files(n: u64) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

/// Share of `total` held, from 0 to 1. An empty transfer is complete.
pub fn fraction(have: u64, total: u64) -> f32 {
    if total == 0 {
        1.0
    } else {
        (have as f64 / total as f64).clamp(0.0, 1.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_the_terminal() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.00 KiB");
        assert_eq!(bytes(9 * 1024 * 1024 + 123), "9.00 MiB");
        assert_eq!(bytes(98_400_000_000), "91.6 GiB");
        assert_eq!(bytes(500 * 1024 * 1024 * 1024), "500 GiB");
    }

    #[test]
    fn time_left_is_rounded_up() {
        assert_eq!(eta(Duration::from_millis(300)), "1 s");
        assert_eq!(eta(Duration::from_secs(61)), "2 min");
        assert_eq!(eta(Duration::from_secs(3600 + 5 * 60)), "1 h 05 min");
    }

    #[test]
    fn counts_and_shares() {
        assert_eq!(files(1), "1 file");
        assert_eq!(files(6), "6 files");
        assert_eq!(fraction(0, 0), 1.0);
        assert_eq!(fraction(5, 10), 0.5);
        assert_eq!(fraction(20, 10), 1.0);
    }
}
