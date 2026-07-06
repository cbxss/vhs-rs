//! Small shared utilities.

use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

/// Creates `path`'s parent directories as needed (a no-op for bare
/// filenames).
///
/// # Errors
/// Returns any error from `create_dir_all` (permissions, or a non-directory
/// component in the way).
pub fn ensure_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Parses a tape duration literal: a decimal number with an optional
/// `ms`/`s`/`m` suffix (no suffix means seconds, matching VHS).
///
/// Examples: `"100ms"`, `"1.5s"`, `".1s"`, `"2m"`, `"3"`.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (num, scale_ms) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000.0)
    } else {
        (s, 1000.0)
    };

    let value: f64 = num.parse().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    // try_from, not from: `from_secs_f64` PANICS on values that overflow
    // Duration (e.g. `Sleep 1e23`), and tapes are untrusted input.
    Duration::try_from_secs_f64(value * scale_ms / 1000.0).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("100ms"), Some(Duration::from_millis(100)));
        assert_eq!(parse_duration("1s"), Some(Duration::from_secs(1)));
        assert_eq!(parse_duration(".5s"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("0.5s"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("1m"), Some(Duration::from_secs(60)));
        assert_eq!(parse_duration("2"), Some(Duration::from_secs(2)));
        assert_eq!(parse_duration("garbage"), None);
        assert_eq!(parse_duration("-1s"), None);
    }

    /// Hostile values must return None, never panic: `from_secs_f64` aborts
    /// the process (panic=abort) on overflow, and tapes are untrusted input.
    #[test]
    fn hostile_durations_are_rejected_not_panics() {
        for s in [
            "999999999999999999999999",   // > Duration::MAX seconds
            "999999999999999999999999ms", // overflows after scaling too
            "99999999999999999999m",
            "1e300",
            "1e400", // parses to f64 infinity
            "inf",
            "NaN",
            "nan",
        ] {
            assert_eq!(parse_duration(s), None, "input {s:?}");
        }
        // The largest representable durations still work.
        assert!(parse_duration("1000000s").is_some());
        assert!(parse_duration("525600m").is_some()); // one year
    }
}
