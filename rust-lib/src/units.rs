//! Token amounts as strings of decimal digits. A swap moves base units, a human types token
//! units, and the shift between them is exact integer work — never a float, which cannot hold
//! 18 decimal places. The one float in this file is the rate line, and it says so.

use std::cmp::Ordering;

/// "1.5" with 6 decimals → "1500000". `None` when the text is not an amount: not digits, more
/// fractional places than the token has, or nothing at all. "0" is a valid amount of nothing.
pub fn to_base_units(units: &str, decimals: u32) -> Option<String> {
    let t = units.trim();
    if t.is_empty() || decimals > 77 {
        return None;
    }
    let (whole, frac) = t.split_once('.').unwrap_or((t, ""));
    if whole.is_empty() && frac.is_empty() {
        return None;
    }
    if !is_digits_or_empty(whole) || !is_digits_or_empty(frac) || frac.len() > decimals as usize {
        return None;
    }
    let digits = format!("{whole}{frac}{}", "0".repeat(decimals as usize - frac.len()));
    Some(strip_leading_zeros(&digits).to_string())
}

/// Every digit of a base-unit amount in token units, with no trailing zeros: "1500000" at 6
/// decimals → "1.5"; "1" at 18 → "0.000000000000000001". `None` for a string that is not digits.
pub fn from_base_units_exact(base: &str, decimals: u32) -> Option<String> {
    let t = base.trim();
    if !is_digits(t) {
        return None;
    }
    let d = decimals as usize;
    let digits = if t.len() <= d { format!("{}{t}", "0".repeat(d - t.len() + 1)) } else { t.to_string() };
    let (whole, frac) = digits.split_at(digits.len() - d);
    let frac = frac.trim_end_matches('0');
    Some(if frac.is_empty() { whole.to_string() } else { format!("{whole}.{frac}") })
}

/// A bounded display: at most five fractional digits, TRUNCATED rather than rounded so a figure
/// never reads as more than it is, and "<0.00001" for an amount that is not nothing but would
/// print as if it were.
pub fn from_base_units(base: &str, decimals: u32) -> Option<String> {
    const PLACES: usize = 5;
    let exact = from_base_units_exact(base, decimals)?;
    let Some(dot) = exact.find('.') else { return Some(exact) };
    if exact.len() - dot - 1 <= PLACES {
        return Some(exact);
    }
    let cut = exact[..dot + 1 + PLACES].trim_end_matches('0').trim_end_matches('.');
    if cut == "0" {
        return Some(format!("<0.{}1", "0".repeat(PLACES - 1)));
    }
    Some(cut.to_string())
}

/// Numeric order of two base-unit amounts. Callers check both are digits first.
pub fn compare_base(a: &str, b: &str) -> Ordering {
    let (x, y) = (strip_leading_zeros(a.trim()), strip_leading_zeros(b.trim()));
    x.len().cmp(&y.len()).then_with(|| x.cmp(y))
}

/// The sum of two base-unit amounts, added digit by digit so it is exact at any size. `None`
/// when either is not digits.
pub fn add_base(a: &str, b: &str) -> Option<String> {
    let (a, b) = (a.trim(), b.trim());
    if !is_digits(a) || !is_digits(b) {
        return None;
    }
    let (mut x, mut y) = (a.bytes().rev(), b.bytes().rev());
    let (mut out, mut carry) = (Vec::with_capacity(a.len().max(b.len()) + 1), 0u8);
    loop {
        let (p, q) = (x.next(), y.next());
        if p.is_none() && q.is_none() && carry == 0 {
            break;
        }
        let sum = p.map_or(0, |c| c - b'0') + q.map_or(0, |c| c - b'0') + carry;
        out.push(b'0' + sum % 10);
        carry = sum / 10;
    }
    out.reverse();
    Some(strip_leading_zeros(std::str::from_utf8(&out).ok()?).to_string())
}

pub fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())
}

/// How many of the token out one token in buys, for the rate line, to six significant digits.
/// A display of a ratio, not an amount that moves — the one place a float is allowed.
pub fn rate_of(amount_in: &str, decimals_in: u32, amount_out: &str, decimals_out: u32) -> Option<String> {
    if !is_digits(amount_in) || !is_digits(amount_out) {
        return None;
    }
    let units = |base: &str, d: u32| from_base_units_exact(base, d).and_then(|s| s.parse::<f64>().ok());
    let (i, o) = (units(amount_in, decimals_in)?, units(amount_out, decimals_out)?);
    if !(i > 0.0) || !(o > 0.0) {
        return None;
    }
    Some(six_significant(o / i))
}

/// `printf("%.6g")`, except that an exponent form is spelled out to twelve places instead.
fn six_significant(r: f64) -> String {
    let exponent: i32 = format!("{r:.5e}").rsplit('e').next().and_then(|e| e.parse().ok()).unwrap_or(0);
    let s = if (-4..6).contains(&exponent) {
        format!("{r:.*}", (5 - exponent) as usize)
    } else {
        format!("{r:.12}")
    };
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

fn strip_leading_zeros(s: &str) -> &str {
    let t = s.trim_start_matches('0');
    if t.is_empty() && !s.is_empty() { &s[s.len() - 1..] } else { t }
}

fn is_digits_or_empty(s: &str) -> bool {
    s.bytes().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(u: &str, d: u32) -> String {
        to_base_units(u, d).unwrap_or_default()
    }

    #[test]
    fn token_units_become_base_units_exactly_or_are_refused() {
        assert_eq!(base("1.5", 6), "1500000");
        assert_eq!(base("1000", 6), "1000000000");
        assert_eq!(base(".5", 18), "500000000000000000");
        assert_eq!(base("5.", 18), "5000000000000000000");
        assert_eq!(base("0.000000000000000001", 18), "1");
        assert_eq!(base("0", 18), "0", "zero is an amount of nothing, not a refusal");
        assert_eq!(base("0.0", 6), "0");
        assert_eq!(base("007", 2), "700");
        assert_eq!(base("  2 ", 0), "2");
        for refused in [("1.1234567", 6), ("ten", 6), ("-1", 6), ("1,5", 6), ("", 6), (".", 6), ("1.2.3", 6)] {
            assert_eq!(to_base_units(refused.0, refused.1), None, "{refused:?}");
        }
    }

    #[test]
    fn base_units_read_back_exactly_and_display_truncated() {
        let exact = |b: &str, d| from_base_units_exact(b, d).unwrap_or_default();
        assert_eq!(exact("1500000", 6), "1.5");
        assert_eq!(exact("1", 18), "0.000000000000000001");
        assert_eq!(exact("5000000000000000000", 18), "5");
        assert_eq!(exact("0", 18), "0");
        assert_eq!(exact("42", 0), "42");
        let shown = |b: &str, d| from_base_units(b, d).unwrap_or_default();
        assert_eq!(shown("333277787035494084", 18), "0.33327");
        assert_eq!(shown("199999", 6), "0.19999", "never rounding up");
        assert_eq!(shown("1", 18), "<0.00001");
        assert_eq!(shown("0", 18), "0");
        assert_eq!(shown("1500000", 6), "1.5");
        assert_eq!(from_base_units("abc", 6), None);
    }

    #[test]
    fn amounts_compare_numerically() {
        assert_eq!(compare_base("999", "1000"), Ordering::Less);
        assert_eq!(compare_base("1000", "999"), Ordering::Greater);
        assert_eq!(compare_base("0001000", "1000"), Ordering::Equal);
        assert_eq!(compare_base("1999", "2000"), Ordering::Less);
        assert_eq!(compare_base("5000000000000", "5000000000001"), Ordering::Less);
        assert_eq!(compare_base("0", "000"), Ordering::Equal);
    }

    #[test]
    fn amounts_add_exactly_past_any_integer_width() {
        assert_eq!(add_base("999", "1").as_deref(), Some("1000"));
        assert_eq!(add_base("0", "0").as_deref(), Some("0"));
        assert_eq!(add_base("007", "3").as_deref(), Some("10"));
        let big = "115792089237316195423570985008687907853269984665640564039457584007913129639935";
        assert_eq!(add_base(big, "1").as_deref(),
                   Some("115792089237316195423570985008687907853269984665640564039457584007913129639936"));
        assert_eq!(add_base("1", "x"), None);
        assert_eq!(add_base("", "1"), None);
    }

    #[test]
    fn the_rate_line_has_six_significant_digits() {
        assert_eq!(rate_of("1000000000", 6, "333277787035494084", 18).as_deref(), Some("0.000333278"));
        assert_eq!(rate_of("333277787035494084", 18, "1000000000", 6).as_deref(), Some("3000.5"));
        assert_eq!(rate_of("0", 6, "5", 18), None);
        assert_eq!(rate_of("x", 6, "5", 18), None);
        assert_eq!(rate_of("1", 0, "1234567", 0).as_deref(), Some("1234567"), "past %g's range");
        assert_eq!(rate_of("100000", 0, "1", 0).as_deref(), Some("0.00001"));
    }
}
