//! Shared zoom parsing; invalid/non-finite drafts preserve the applied value.
pub(crate) fn zoom(action: &str, current: f32, minimum: f32, maximum: f32) -> f32 {
    let requested = match action {
        "zoom-in" => current * 1.2,
        "zoom-out" => current / 1.2,
        "zoom-reset" => 1.0,
        _ => action
            .strip_prefix("zoom-set:")
            .and_then(|value| {
                value
                    .trim()
                    .trim_end_matches('%')
                    .trim()
                    .parse::<f32>()
                    .ok()
            })
            .filter(|value| value.is_finite())
            .map(|value| value / 100.0)
            .unwrap_or(current),
    };
    requested.clamp(minimum, maximum)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn percentage_drafts_and_limits() {
        assert_eq!(zoom("zoom-set: 125% ", 1.0, 0.1, 3.0), 1.25);
        for invalid in ["NaN", "inf", "", "oops"] {
            assert_eq!(zoom(&format!("zoom-set:{invalid}"), 1.25, 0.1, 3.0), 1.25);
        }
        assert_eq!(zoom("zoom-set:900", 1.0, 0.1, 3.0), 3.0);
    }
}
