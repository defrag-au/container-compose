//! Durations, in the four units the design accepts: `ms`, `s`, `m`, `h`.
//!
//! An integer and a unit are both required. There is no bare-number form,
//! because `interval = 30` is ambiguous and the runtime's own `--time` is in
//! seconds while a healthcheck interval is not.

use std::fmt;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span(Duration);

impl Span {
    pub fn from_millis(millis: u64) -> Self {
        Self(Duration::from_millis(millis))
    }

    pub fn as_duration(self) -> Duration {
        self.0
    }

    pub fn millis(self) -> u128 {
        self.0.as_millis()
    }

    pub fn is_zero(self) -> bool {
        self.0.is_zero()
    }

    pub fn parse(input: &str) -> Result<Self, String> {
        if input.is_empty() {
            return Err("a duration is required, such as `15s`".to_owned());
        }
        if input.chars().any(char::is_whitespace) {
            return Err(format!(
                "`{input}` contains whitespace; write it as `15s`, not `15 s`"
            ));
        }
        let digits = input
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(input.len());
        if digits == 0 {
            return Err(format!("`{input}` must start with a number"));
        }
        let (number, unit) = input.split_at(digits);
        let value: u64 = number
            .parse()
            .map_err(|_| format!("`{number}` is not a number"))?;
        let millis = match unit {
            "ms" => value,
            "s" => value.checked_mul(1_000).ok_or("too long")?,
            "m" => value.checked_mul(60_000).ok_or("too long")?,
            "h" => value.checked_mul(3_600_000).ok_or("too long")?,
            other => {
                return Err(format!("unknown unit `{other}`; use `ms`, `s`, `m` or `h`"));
            }
        };
        Ok(Self(Duration::from_millis(millis)))
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let millis = self.millis();
        if millis.is_multiple_of(3_600_000) && millis >= 3_600_000 {
            write!(f, "{}h", millis / 3_600_000)
        } else if millis.is_multiple_of(60_000) && millis >= 60_000 {
            write!(f, "{}m", millis / 60_000)
        } else if millis.is_multiple_of(1_000) {
            write!(f, "{}s", millis / 1_000)
        } else {
            write!(f, "{millis}ms")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Span;

    #[test]
    fn accepts_the_four_units() {
        assert_eq!(Span::parse("250ms").unwrap().millis(), 250);
        assert_eq!(Span::parse("15s").unwrap().millis(), 15_000);
        assert_eq!(Span::parse("2m").unwrap().millis(), 120_000);
        assert_eq!(Span::parse("1h").unwrap().millis(), 3_600_000);
    }

    #[test]
    fn rejects_a_bare_number_and_an_unknown_unit() {
        assert!(Span::parse("30").is_err());
        assert!(Span::parse("30 s").is_err());
        assert!(Span::parse("30d").is_err());
    }

    #[test]
    fn displays_the_largest_whole_unit() {
        assert_eq!(Span::parse("1500ms").unwrap().to_string(), "1500ms");
        assert_eq!(Span::parse("15s").unwrap().to_string(), "15s");
        assert_eq!(Span::parse("120s").unwrap().to_string(), "2m");
    }
}
