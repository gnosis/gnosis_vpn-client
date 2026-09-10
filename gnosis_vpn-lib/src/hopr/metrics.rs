/// Extract a gauge value for a specific session from Prometheus text exposition format.
pub(crate) fn gauge_value(text: &str, metric: &str, session_id: &str) -> Option<f64> {
    let label = format!("session_id=\"{session_id}\"");
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix(metric) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix('{') else {
            continue;
        };
        let Some((labels, value)) = rest.split_once('}') else {
            continue;
        };
        if !labels.contains(&label) {
            continue;
        }
        return value.trim().parse().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::gauge_value;

    const FIXTURE: &str = r#"
# HELP hopr_session_surb_buffer_estimate Estimated SURB buffer size
# TYPE hopr_session_surb_buffer_estimate gauge
hopr_session_surb_buffer_estimate{session_id="aabbcc"} 12345
hopr_session_surb_buffer_estimate{session_id="ddeeff"} 777
hopr_session_surb_rate_per_sec{session_id="aabbcc"} 512.5
hopr_session_surb_produced_total{session_id="aabbcc",other="x"} 123456
"#;

    #[test]
    fn finds_value_for_matching_session() {
        let v = gauge_value(FIXTURE, "hopr_session_surb_buffer_estimate", "aabbcc");
        assert_eq!(v, Some(12345.0));
        let v = gauge_value(FIXTURE, "hopr_session_surb_buffer_estimate", "ddeeff");
        assert_eq!(v, Some(777.0));
    }

    #[test]
    fn parses_fractional_values() {
        let v = gauge_value(FIXTURE, "hopr_session_surb_rate_per_sec", "aabbcc");
        assert_eq!(v, Some(512.5));
    }

    #[test]
    fn matches_with_additional_labels() {
        let v = gauge_value(FIXTURE, "hopr_session_surb_produced_total", "aabbcc");
        assert_eq!(v, Some(123456.0));
    }

    #[test]
    fn misses_unknown_metric_or_session() {
        assert_eq!(gauge_value(FIXTURE, "hopr_session_surb_buffer_estimate", "nope"), None);
        assert_eq!(gauge_value(FIXTURE, "hopr_unknown_metric", "aabbcc"), None);
    }
}
