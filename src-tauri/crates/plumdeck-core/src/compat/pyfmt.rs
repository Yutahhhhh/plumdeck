//! Python number formatting and arithmetic: `repr(float)`, `format(x, spec)`,
//! `'%.2f' % x`, `round`, `int`, `//`, `%` and `statistics.median`.
//!
//! Rust's `{:.N}` / `{:.Ne}` formatting is exact and rounds ties to even, the
//! same as Python's dtoa, so digit generation is delegated to it and only the
//! layout rules are reimplemented here.

use std::cmp::Ordering;
use std::fmt;

/// The Python exception a numeric helper would have raised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PyNumError {
    /// `ValueError` (e.g. `int(nan)`).
    ValueError,
    /// `OverflowError` (e.g. `int(inf)`).
    OverflowError,
    /// `ZeroDivisionError`.
    ZeroDivisionError,
    /// `statistics.StatisticsError` (median of no data).
    StatisticsError,
    /// The Python result is a valid int that does not fit in `i64`.
    OutOfRange,
    /// The format spec is outside the supported subset.
    UnsupportedSpec(String),
}

impl PyNumError {
    /// Python's exception class name.
    pub fn python_name(&self) -> &str {
        match self {
            Self::ValueError | Self::UnsupportedSpec(_) => "ValueError",
            Self::OverflowError | Self::OutOfRange => "OverflowError",
            Self::ZeroDivisionError => "ZeroDivisionError",
            Self::StatisticsError => "StatisticsError",
        }
    }
}

impl fmt::Display for PyNumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSpec(spec) => write!(f, "unsupported format spec {spec:?}"),
            other => f.write_str(other.python_name()),
        }
    }
}

impl std::error::Error for PyNumError {}

/// Shortest round-trip digits of a finite `|x|` and the decimal exponent of
/// the first digit (`x = d.ddd * 10^exp`).
///
/// When two shortest candidates are equally near (`190.246002197265625` has
/// `...562` and `...563`), Python's dtoa takes the even one and Rust's the
/// upper one. The correctly rounded (ties-to-even) string of the same length
/// is Python's answer whenever it still round-trips.
fn shortest_digits(x: f64) -> (String, i32) {
    let ax = x.abs();
    let (digits, exp) = split_exp(&format!("{:e}", ax));
    if digits.len() > 1 {
        let nearest = format!("{:.*e}", digits.len() - 1, ax);
        if nearest.parse::<f64>() == Ok(ax) {
            let (mut digits, exp) = split_exp(&nearest);
            while digits.len() > 1 && digits.ends_with('0') {
                digits.pop();
            }
            return (digits, exp);
        }
    }
    (digits, exp)
}

/// Splits Rust's `1.2345e-5` into (`"12345"`, -5).
fn split_exp(text: &str) -> (String, i32) {
    let (mantissa, exp) = text.split_once('e').expect("exponent form");
    (mantissa.replace('.', ""), exp.parse().expect("exponent"))
}

fn push_exponent(out: &mut String, exp: i32) {
    out.push(if exp < 0 { '-' } else { '+' });
    let magnitude = exp.unsigned_abs();
    if magnitude < 10 {
        out.push('0');
    }
    out.push_str(&magnitude.to_string());
}

/// `repr(x)` / `str(x)` for a Python float.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.into();
    }
    let (digits, exp) = shortest_digits(x);
    let decpt = exp + 1;
    let mut out = String::with_capacity(digits.len() + 8);
    if x.is_sign_negative() {
        out.push('-');
    }
    if decpt <= -4 || decpt > 16 {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        push_exponent(&mut out, exp);
    } else if decpt <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-decpt) as usize));
        out.push_str(&digits);
    } else if decpt as usize >= digits.len() {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', decpt as usize - digits.len()));
        out.push_str(".0");
    } else {
        out.push_str(&digits[..decpt as usize]);
        out.push('.');
        out.push_str(&digits[decpt as usize..]);
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sign {
    Minus,
    Plus,
    Space,
}

#[derive(Clone, Copy, Debug)]
struct Spec {
    sign: Sign,
    precision: Option<usize>,
    kind: char,
}

fn parse_spec(spec: &str) -> Result<Spec, PyNumError> {
    let unsupported = || PyNumError::UnsupportedSpec(spec.to_owned());
    let mut rest = spec;
    let sign = match rest.chars().next() {
        Some('+') => Sign::Plus,
        Some('-') => Sign::Minus,
        Some(' ') => Sign::Space,
        _ => Sign::Minus,
    };
    if matches!(rest.chars().next(), Some('+' | '-' | ' ')) {
        rest = &rest[1..];
    }
    let mut precision = None;
    if let Some(after_dot) = rest.strip_prefix('.') {
        let digits = after_dot.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return Err(unsupported());
        }
        precision = Some(after_dot[..digits].parse().map_err(|_| unsupported())?);
        rest = &after_dot[digits..];
    }
    let kind = match rest {
        "f" | "F" | "e" | "E" | "g" | "G" => rest.chars().next().unwrap(),
        _ => return Err(unsupported()),
    };
    Ok(Spec { sign, precision, kind })
}

fn strip_fraction_zeros(text: &mut String) {
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
}

fn format_exp(ax: f64, precision: usize, strip: bool) -> String {
    let (mantissa, exp) = format!("{:.*e}", precision, ax)
        .split_once('e')
        .map(|(m, e)| (m.to_owned(), e.parse::<i32>().expect("exponent")))
        .expect("exponent form");
    let mut out = mantissa;
    if strip {
        strip_fraction_zeros(&mut out);
    }
    out.push('e');
    push_exponent(&mut out, exp);
    out
}

fn format_general(ax: f64, precision: usize) -> String {
    let p = precision.max(1);
    let (_, exp) = split_exp(&format!("{:.*e}", p - 1, ax));
    if -4 <= exp && exp < p as i32 {
        let mut out = format!("{:.*}", (p as i32 - 1 - exp) as usize, ax);
        strip_fraction_zeros(&mut out);
        out
    } else {
        format_exp(ax, p - 1, true)
    }
}

/// `format(x, spec)` for a float, with spec `[+- ][.precision](f|F|e|E|g|G)`.
///
/// This is also `f"{x:spec}"`; `f"{n:g}"` on a Python int formats `float(n)`.
pub fn format_float(x: f64, spec: &str) -> Result<String, PyNumError> {
    let spec = parse_spec(spec)?;
    let upper = spec.kind.is_ascii_uppercase();
    let body = if x.is_nan() {
        "nan".to_owned()
    } else if x.is_infinite() {
        "inf".to_owned()
    } else {
        let ax = x.abs();
        match spec.kind.to_ascii_lowercase() {
            'f' => format!("{:.*}", spec.precision.unwrap_or(6), ax),
            'e' => format_exp(ax, spec.precision.unwrap_or(6), false),
            _ => format_general(ax, spec.precision.unwrap_or(6)),
        }
    };
    let body = if upper { body.to_ascii_uppercase() } else { body };
    let negative = x.is_sign_negative() && !x.is_nan();
    let prefix = match (negative, spec.sign) {
        (true, _) => "-",
        (false, Sign::Plus) => "+",
        (false, Sign::Space) => " ",
        (false, Sign::Minus) => "",
    };
    Ok(format!("{prefix}{body}"))
}

/// Old-style `'%.2f' % x`. Accepts `%[+- ][.precision](f|F|e|E|g|G)`.
pub fn percent_format(spec: &str, x: f64) -> Result<String, PyNumError> {
    let inner = spec
        .strip_prefix('%')
        .ok_or_else(|| PyNumError::UnsupportedSpec(spec.to_owned()))?;
    format_float(x, inner)
}

/// `round(x)` with no ndigits: round half to even, returned as an int.
pub fn round_to_int(x: f64) -> Result<i64, PyNumError> {
    float_to_int(x.round_ties_even())
}

/// `round(x, ndigits)` for `ndigits >= 0`: the correctly rounded decimal
/// (ties to even on the exact binary value) parsed back to a float.
pub fn round_ndigits(x: f64, ndigits: u32) -> f64 {
    // CPython returns x unchanged past the last representable decimal digit.
    const NDIGITS_MAX: u32 = 323;
    if !x.is_finite() || ndigits > NDIGITS_MAX {
        return x;
    }
    format!("{:.*}", ndigits as usize, x).parse().expect("formatted float")
}

/// `int(x)` for a float: truncation toward zero.
pub fn int_trunc(x: f64) -> Result<i64, PyNumError> {
    float_to_int(x.trunc())
}

fn float_to_int(integral: f64) -> Result<i64, PyNumError> {
    if integral.is_nan() {
        return Err(PyNumError::ValueError);
    }
    if integral.is_infinite() {
        return Err(PyNumError::OverflowError);
    }
    // 2^63 is exactly representable; everything below it fits.
    if integral >= 9_223_372_036_854_775_808.0 || integral < -9_223_372_036_854_775_808.0 {
        return Err(PyNumError::OutOfRange);
    }
    Ok(integral as i64)
}

/// `a // b` and `a % b` for Python floats (CPython's `_float_div_mod`).
pub fn float_divmod(a: f64, b: f64) -> Result<(f64, f64), PyNumError> {
    if b == 0.0 {
        return Err(PyNumError::ZeroDivisionError);
    }
    let mut modulo = a % b;
    let mut div = (a - modulo) / b;
    if modulo != 0.0 {
        if (b < 0.0) != (modulo < 0.0) {
            modulo += b;
            div -= 1.0;
        }
    } else {
        modulo = 0.0f64.copysign(b);
    }
    let floordiv = if div != 0.0 {
        let mut floordiv = div.floor();
        if div - floordiv > 0.5 {
            floordiv += 1.0;
        }
        floordiv
    } else {
        0.0f64.copysign(a / b)
    };
    Ok((floordiv, modulo))
}

/// Python float `a // b`.
pub fn float_floordiv(a: f64, b: f64) -> Result<f64, PyNumError> {
    float_divmod(a, b).map(|(div, _)| div)
}

/// Python float `a % b` (CPython's `float_rem`; the sign follows `b`).
pub fn float_mod(a: f64, b: f64) -> Result<f64, PyNumError> {
    if b == 0.0 {
        return Err(PyNumError::ZeroDivisionError);
    }
    let modulo = a % b;
    Ok(if modulo != 0.0 {
        if (b < 0.0) != (modulo < 0.0) {
            modulo + b
        } else {
            modulo
        }
    } else {
        0.0f64.copysign(b)
    })
}

/// Python int `a // b` (floor division; differs from `div_euclid` for b < 0).
pub fn int_floordiv(a: i64, b: i64) -> Result<i64, PyNumError> {
    if b == 0 {
        return Err(PyNumError::ZeroDivisionError);
    }
    let quotient = a.checked_div(b).ok_or(PyNumError::OutOfRange)?;
    Ok(if a % b != 0 && ((a < 0) != (b < 0)) { quotient - 1 } else { quotient })
}

/// Python int `a % b` (the result takes the sign of `b`).
pub fn int_mod(a: i64, b: i64) -> Result<i64, PyNumError> {
    if b == 0 {
        return Err(PyNumError::ZeroDivisionError);
    }
    let remainder = a.checked_rem(b).unwrap_or(0);
    Ok(if remainder != 0 && ((remainder < 0) != (b < 0)) { remainder + b } else { remainder })
}

/// `statistics.median` over floats: the middle value, or the mean of the two
/// middle values. NaN inputs are not supported (Python's order is undefined).
pub fn median(values: &[f64]) -> Result<f64, PyNumError> {
    if values.is_empty() {
        return Err(PyNumError::StatisticsError);
    }
    let mut sorted = values.to_vec();
    // Stable, with -0.0 == 0.0, exactly like Python's sorted().
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let n = sorted.len();
    Ok(if n % 2 == 1 { sorted[n / 2] } else { (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repr_layout() {
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1e15), "1000000000000000.0");
        assert_eq!(float_repr(1e-5), "1e-05");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(float_repr(-0.0), "-0.0");
        assert_eq!(float_repr(128.0), "128.0");
        assert_eq!(float_repr(127.9f32 as f64), "127.9000015258789");
        assert_eq!(float_repr(f64::from_bits(0x4067c7df40000000)), "190.24600219726562");
    }

    #[test]
    fn general_and_signed() {
        assert_eq!(format_float(126.0, "g").unwrap(), "126");
        assert_eq!(format_float(-2.0, "+g").unwrap(), "-2");
        assert_eq!(format_float(2.5, "+.1f").unwrap(), "+2.5");
        assert_eq!(format_float(1e16, "g").unwrap(), "1e+16");
        assert_eq!(percent_format("%.2f", 126.25).unwrap(), "126.25");
        assert!(format_float(1.0, ",d").is_err());
    }

    #[test]
    fn floor_division_follows_divisor_sign() {
        assert_eq!(int_floordiv(7, -2), Ok(-4));
        assert_eq!(int_mod(7, -2), Ok(-1));
        assert_eq!(int_mod(-7, 2), Ok(1));
        assert_eq!(float_mod(-1.0, 3.0), Ok(2.0));
        assert_eq!(int_floordiv(i64::MIN, -1), Err(PyNumError::OutOfRange));
    }
}

/// `float(text)` for a `str`: surrounding whitespace (`str.strip()`) is
/// ignored, underscores are allowed only between digits (PEP 515), and
/// `inf`, `infinity` and `nan` are accepted in any case. `None` is the
/// `ValueError`.
pub fn py_float_from_str(text: &str) -> Option<f64> {
    let trimmed = crate::compat::text::py_strip(text);
    let chars: Vec<char> = trimmed.chars().collect();
    let mut cleaned = String::with_capacity(trimmed.len());
    for (index, &c) in chars.iter().enumerate() {
        if c == '_' {
            let digit = |at: Option<&char>| at.is_some_and(|c| c.is_ascii_digit());
            if !(index > 0 && digit(chars.get(index - 1)) && digit(chars.get(index + 1))) {
                return None;
            }
            continue;
        }
        if !c.is_ascii() {
            return None;
        }
        cleaned.push(c);
    }
    cleaned.parse().ok()
}

#[cfg(test)]
mod float_from_str_tests {
    use super::py_float_from_str;

    #[test]
    fn follows_python_float() {
        assert_eq!(py_float_from_str(" 1_000.5\n"), Some(1000.5));
        assert_eq!(py_float_from_str("1__0"), None);
        assert_eq!(py_float_from_str("_1"), None);
        assert_eq!(py_float_from_str("-InFiNiTy"), Some(f64::NEG_INFINITY));
        assert!(py_float_from_str("nan").unwrap().is_nan());
        assert_eq!(py_float_from_str(".5e1"), Some(5.0));
        assert_eq!(py_float_from_str("1e"), None);
        assert_eq!(py_float_from_str(""), None);
        assert_eq!(py_float_from_str("0x10"), None);
    }
}
