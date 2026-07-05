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
    Some(Duration::from_secs_f64(value * scale_ms / 1000.0))
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
}
