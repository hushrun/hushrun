//! How a run's lines say numbers and times, in the words people read: what the code every adapter
//! shares and the CLI both say. Apart from `cmd/`, so that the shared code reaches none of the
//! CLI's files (CONTRIBUTING.md, "Which tests CI runs again").

/// `1,234,567`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A run's length, rounded for people: `0.4s`, `41s`, `3m 12s`, `1h 4m`.
pub fn took(ms: u64) -> String {
    let s = ms / 1000;
    match ms {
        0..1_000 => format!("{:.1}s", ms as f64 / 1000.0),
        1_000..60_000 => format!("{s}s"),
        60_000..3_600_000 => format!("{}m {}s", s / 60, s % 60),
        _ => format!("{}h {}m", s / 3_600, s / 60 % 60),
    }
}
