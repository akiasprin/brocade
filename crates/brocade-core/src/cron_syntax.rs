//! Pure structural checks for the five-field robfig/cron grammar used by Xray. Console also
//! asks the embedded engine to resolve timezones and parse the expression before a write.
pub fn valid_cron(input: &str) -> bool {
    let mut input = input.trim();
    if input.starts_with("TZ=") || input.starts_with("CRON_TZ=") {
        let Some((zone, rest)) = input.split_once(' ') else {
            return false;
        };
        let zone = zone.split_once('=').map(|(_, z)| z).unwrap_or_default();
        if zone.is_empty()
            || !zone
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_-+".contains(&b))
        {
            return false;
        }
        input = rest.trim();
    }
    if matches!(
        input,
        "@yearly" | "@annually" | "@monthly" | "@weekly" | "@daily" | "@midnight" | "@hourly"
    ) {
        return true;
    }
    if let Some(value) = input.strip_prefix("@every ") {
        return duration(value).is_some_and(|seconds| seconds >= 60.0);
    }
    let fields = input.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        return false;
    }
    [(0, 59), (0, 23), (1, 31), (1, 12), (0, 6)]
        .into_iter()
        .enumerate()
        .all(|(index, (min, max))| {
            fields[index]
                .split(',')
                .filter(|v| !v.is_empty())
                .all(|value| {
                    let mut step = value.split('/');
                    let range = step.next().unwrap_or_default();
                    if let Some(increment) = step.next() {
                        if increment.parse::<u64>().ok().is_none_or(|n| n == 0) {
                            return false;
                        }
                    }
                    if step.next().is_some() {
                        return false;
                    }
                    if matches!(range, "*" | "?") {
                        return true;
                    }
                    let number = |raw: &str| -> Option<u64> {
                        let names = if index == 3 {
                            &[
                                "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep",
                                "oct", "nov", "dec",
                            ][..]
                        } else if index == 4 {
                            &["sun", "mon", "tue", "wed", "thu", "fri", "sat"][..]
                        } else {
                            &[][..]
                        };
                        names
                            .iter()
                            .position(|name| raw.eq_ignore_ascii_case(name))
                            .map(|n| n as u64 + if index == 3 { 1 } else { 0 })
                            .or_else(|| raw.parse::<u64>().ok())
                    };
                    let mut ends = range.split('-');
                    let Some(first) = number(ends.next().unwrap_or_default()) else {
                        return false;
                    };
                    let last = match ends.next() {
                        Some(v) => match number(v) {
                            Some(n) => n,
                            None => return false,
                        },
                        None => first,
                    };
                    ends.next().is_none() && first >= min && first <= last && last <= max
                })
        })
}
fn duration(mut value: &str) -> Option<f64> {
    let mut total = 0.0;
    while !value.is_empty() {
        let end = value
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(value.len());
        if end == 0 {
            return None;
        }
        let number = value[..end].parse::<f64>().ok()?;
        value = &value[end..];
        let (unit, scale) = [
            ("ms", 0.001),
            ("us", 0.000001),
            ("µs", 0.000001),
            ("μs", 0.000001),
            ("ns", 0.000000001),
            ("h", 3600.0),
            ("m", 60.0),
            ("s", 1.0),
        ]
        .into_iter()
        .find(|(unit, _)| value.starts_with(unit))?;
        value = &value[unit.len()..];
        total += number * scale;
    }
    (total > 0.0 && total.is_finite()).then_some(total)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_xray_schedules_and_rejects_invalid_ranges() {
        for value in [
            "CRON_TZ=Asia/Shanghai 30 6 * * *",
            "TZ=UTC @weekly",
            "@every 1h30m",
            "@daily",
            "0 6 * JAN MON-FRI",
            "*/15 * * * ?",
        ] {
            assert!(valid_cron(value), "{value}");
        }
        for value in [
            "",
            "nope",
            "60 * * * *",
            "0 0 * * 7",
            "*/0 * * * *",
            "0 0 31-1 * *",
            "@every 30s",
            "TZ=UTC",
        ] {
            assert!(!valid_cron(value), "{value}");
        }
    }
}
