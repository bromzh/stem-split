use std::{fmt, str::FromStr};

const DIGEST_BYTES: usize = 32;
const HEX_DIGITS: usize = DIGEST_BYTES * 2;

/// A SHA-256 digest: the expected or computed fingerprint of a model file.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; DIGEST_BYTES]);

/// Why a string is not a hexadecimal SHA-256 digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DigestParseError {
    #[error("a SHA-256 digest has {HEX_DIGITS} hexadecimal digits, found {actual} bytes")]
    WrongLength { actual: usize },
    #[error("byte at position {position} is not a hexadecimal digit")]
    InvalidDigit { position: usize },
}

impl Sha256Digest {
    #[must_use]
    pub const fn new(bytes: [u8; DIGEST_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }
}

const fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

impl FromStr for Sha256Digest {
    type Err = DigestParseError;

    /// Parses 64 hexadecimal digits in either case.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let digits: &[u8; HEX_DIGITS] = text
            .as_bytes()
            .try_into()
            .map_err(|_| DigestParseError::WrongLength { actual: text.len() })?;
        let (pairs, _) = digits.as_chunks::<2>();

        let mut bytes = [0_u8; DIGEST_BYTES];
        for (index, (byte, &[high, low])) in bytes.iter_mut().zip(pairs).enumerate() {
            let position = index * 2;
            let high = hex_value(high).ok_or(DigestParseError::InvalidDigit { position })?;
            let low = hex_value(low).ok_or(DigestParseError::InvalidDigit {
                position: position + 1,
            })?;
            *byte = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for Sha256Digest {
    /// Writes the digest as 64 lowercase hexadecimal digits.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .iter()
            .try_for_each(|byte| write!(formatter, "{byte:02x}"))
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Sha256Digest({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Digest of the `htdemucs` graph in the model repository.
    const SAMPLE: &str = "abaac48bc8fd55fa672268c8f13228b9b00f52f0c4cd10945048982eca4643bc";

    #[test]
    fn hex_round_trips() -> Result<(), DigestParseError> {
        let digest: Sha256Digest = SAMPLE.parse()?;
        assert_eq!(digest.to_string(), SAMPLE);
        assert_eq!(digest.as_bytes().first(), Some(&0xab));
        assert_eq!(digest.as_bytes().last(), Some(&0xbc));
        Ok(())
    }

    #[test]
    fn uppercase_is_accepted_and_normalized() -> Result<(), DigestParseError> {
        let digest: Sha256Digest = SAMPLE.to_ascii_uppercase().parse()?;
        assert_eq!(digest, SAMPLE.parse()?);
        assert_eq!(digest.to_string(), SAMPLE);
        Ok(())
    }

    #[test]
    fn wrong_length_is_rejected() {
        assert_eq!(
            "abcd".parse::<Sha256Digest>(),
            Err(DigestParseError::WrongLength { actual: 4 })
        );
        assert_eq!(
            "".parse::<Sha256Digest>(),
            Err(DigestParseError::WrongLength { actual: 0 })
        );
        let too_long = format!("{SAMPLE}00");
        assert_eq!(
            too_long.parse::<Sha256Digest>(),
            Err(DigestParseError::WrongLength { actual: 66 })
        );
    }

    #[test]
    fn non_hex_digit_is_reported_with_its_position() {
        for position in [0, 1, 37, 63] {
            let mut text = SAMPLE.as_bytes().to_vec();
            if let Some(byte) = text.get_mut(position) {
                *byte = b'g';
            }
            let text = String::from_utf8_lossy(&text);
            assert_eq!(
                text.parse::<Sha256Digest>(),
                Err(DigestParseError::InvalidDigit { position })
            );
        }
    }

    #[test]
    fn non_ascii_text_of_the_right_byte_length_is_rejected() {
        // 32 two-byte characters: 64 bytes, none of them a hexadecimal digit.
        let text = "é".repeat(32);
        assert_eq!(
            text.parse::<Sha256Digest>(),
            Err(DigestParseError::InvalidDigit { position: 0 })
        );
    }

    #[test]
    fn debug_shows_the_hex_form() {
        let digest = Sha256Digest::new([0x0f; 32]);
        assert_eq!(
            format!("{digest:?}"),
            format!("Sha256Digest({})", "0f".repeat(32))
        );
    }
}
