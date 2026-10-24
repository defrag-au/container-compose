//! Sizes, as the runtime documents them.
//!
//! The runtime accepts an integer with an optional K, M, G, T or P suffix, and
//! guarantees 1 MiB granularity. The binary spellings (`KiB` and friends) are
//! accepted because they appear in the wild and normalise to the same numbers;
//! a space (`1 GiB`) is not, because the runtime would not parse it either.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Size(u64);

impl Size {
    pub fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    pub fn bytes(self) -> u64 {
        self.0
    }

    /// Parse a runtime size string. The error is a sentence, not an enum: it is
    /// always wrapped in a key path by the caller.
    pub fn parse(input: &str) -> Result<Self, String> {
        if input.is_empty() {
            return Err("a size is required, such as `200G`".to_owned());
        }
        if input.chars().any(char::is_whitespace) {
            return Err(format!(
                "`{input}` contains whitespace; write it as `1G`, not `1 G`"
            ));
        }
        let digits = input
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(input.len());
        if digits == 0 {
            return Err(format!("`{input}` must start with a number"));
        }
        let (number, suffix) = input.split_at(digits);
        let value: u64 = number
            .parse()
            .map_err(|_| format!("`{number}` is not a number"))?;
        let multiplier: u64 = match suffix.to_ascii_lowercase().as_str() {
            "" | "b" => 1,
            "k" | "kb" | "kib" => 1 << 10,
            "m" | "mb" | "mib" => 1 << 20,
            "g" | "gb" | "gib" => 1 << 30,
            "t" | "tb" | "tib" => 1 << 40,
            "p" | "pb" | "pib" => 1 << 50,
            other => {
                return Err(format!(
                    "unknown suffix `{other}`; use K, M, G, T or P (as in `200G`)"
                ));
            }
        };
        value
            .checked_mul(multiplier)
            .map(Size)
            .ok_or_else(|| format!("`{input}` is too large"))
    }
}

impl fmt::Display for Size {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (unit, multiplier) in [
            ("P", 1u64 << 50),
            ("T", 1u64 << 40),
            ("G", 1u64 << 30),
            ("M", 1u64 << 20),
            ("K", 1u64 << 10),
        ] {
            if self.0 >= multiplier && self.0.is_multiple_of(multiplier) {
                return write!(f, "{}{unit}", self.0 / multiplier);
            }
        }
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::Size;

    #[test]
    fn accepts_the_documented_forms() {
        assert_eq!(Size::parse("512M").unwrap().bytes(), 512 << 20);
        assert_eq!(Size::parse("1G").unwrap().bytes(), 1 << 30);
        assert_eq!(Size::parse("10g").unwrap().bytes(), 10 << 30);
        assert_eq!(Size::parse("512MiB").unwrap().bytes(), 512 << 20);
        assert_eq!(Size::parse("1024").unwrap().bytes(), 1024);
    }

    #[test]
    fn rejects_a_space_and_a_decimal() {
        assert!(Size::parse("1 GiB").is_err());
        assert!(Size::parse("1.5G").is_err());
        assert!(Size::parse("G").is_err());
        assert!(Size::parse("").is_err());
    }

    #[test]
    fn displays_the_largest_exact_unit() {
        assert_eq!(Size::parse("200G").unwrap().to_string(), "200G");
        assert_eq!(Size::parse("512M").unwrap().to_string(), "512M");
        assert_eq!(Size::from_bytes(1536).to_string(), "1536");
    }
}
