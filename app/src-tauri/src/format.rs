//! Number and time text for the tray menu; mirrors `app/src/lib/format.ts`.

pub fn compact(value: f64) -> String {
    let abs = value.abs();
    let sign = if value < 0.0 { "-" } else { "" };
    if abs < 1e4 {
        return format!("{sign}{}", grouped(abs.round() as u64));
    }
    const UNITS: [(f64, &str); 4] = [(1e3, "K"), (1e6, "M"), (1e9, "B"), (1e12, "T")];
    let mut index = if abs >= 1e12 {
        3
    } else if abs >= 1e9 {
        2
    } else if abs >= 1e6 {
        1
    } else {
        0
    };
    loop {
        let (divisor, suffix) = UNITS[index];
        let scaled = abs / divisor;
        let digits = if scaled >= 100.0 {
            0
        } else if scaled >= 10.0 {
            1
        } else {
            2
        };
        let text = format!("{scaled:.digits$}");
        if text.parse::<f64>().unwrap_or(0.0) >= 1000.0 && index < UNITS.len() - 1 {
            index += 1;
            continue;
        }
        let trimmed = if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.')
        } else {
            &text
        };
        return format!("{sign}{trimmed}{suffix}");
    }
}

fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub fn relative(now_ms: i64, then_ms: Option<i64>) -> String {
    let Some(then) = then_ms else {
        return "从未".to_owned();
    };
    let seconds = (now_ms - then).max(0) / 1000;
    match seconds {
        0..60 => "刚刚".to_owned(),
        60..3600 => format!("{} 分钟前", seconds / 60),
        3600..86_400 => format!("{} 小时前", seconds / 3600),
        _ => format!("{} 天前", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_matches_the_dashboard() {
        assert_eq!(compact(0.0), "0");
        assert_eq!(compact(9_999.0), "9,999");
        assert_eq!(compact(10_000.0), "10K");
        assert_eq!(compact(190_000.0), "190K");
        assert_eq!(compact(1_234_567.0), "1.23M");
        assert_eq!(compact(999_999.0), "1M");
        assert_eq!(compact(12_300_000_000.0), "12.3B");
        assert_eq!(compact(-25_000.0), "-25K");
    }

    #[test]
    fn relative_time_is_coarse() {
        let now = 10_000_000;
        assert_eq!(relative(now, None), "从未");
        assert_eq!(relative(now, Some(now - 5_000)), "刚刚");
        assert_eq!(relative(now, Some(now + 5_000)), "刚刚");
        assert_eq!(relative(now, Some(now - 125_000)), "2 分钟前");
        assert_eq!(relative(now, Some(now - 7_200_000)), "2 小时前");
        assert_eq!(relative(now, Some(now - 3 * 86_400_000)), "3 天前");
    }
}
