//! sha256 digests: the one check for a sha256 in hex, and the canonical OCI image digest.
//!
//! A digest is lowercase. The OCI image spec registers `sha256` with the encoded form
//! `[a-f0-9]{64}`, and registries answer digests in that form, so uppercase hex is not a sha256
//! here. A reference a person types may spell its digest in uppercase:
//! [`crate::images::ImageRef::parse`] lowercases it before this check, so every digest it yields
//! is canonical.

use std::fmt;
use std::str::FromStr;

/// The length of a sha256 in hex.
pub(crate) const SHA256_HEX_LEN: usize = 64;

/// Whether `hex` is 64 lowercase hexadecimal digits: a canonical sha256 in hex. The one check for
/// a sha256 (an image digest's hex, a source tree hash, a context checksum, a plugin's checksum).
pub fn is_sha256_hex(hex: &str) -> bool {
    hex.len() == SHA256_HEX_LEN
        && hex.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A canonical OCI SHA-256 image digest (`sha256:<64 lowercase hex>`), as ECR and registries
/// report it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ImageDigest(String);

impl ImageDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ImageDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ImageDigest {
    type Err = ImageDigestError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(ImageDigestError::UnsupportedAlgorithm);
        };
        if hex.len() != SHA256_HEX_LEN {
            return Err(ImageDigestError::WrongLength(hex.len()));
        }
        if !is_sha256_hex(hex) {
            return Err(ImageDigestError::NonCanonicalHex);
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ImageDigestError {
    #[error("image digest must use the sha256 algorithm")]
    UnsupportedAlgorithm,
    #[error("sha256 image digest must be 64 hexadecimal characters, found {0} bytes")]
    WrongLength(usize),
    #[error("sha256 image digest must use canonical lowercase hexadecimal characters")]
    NonCanonicalHex,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sha256_is_64_lowercase_hex_digits() {
        assert!(is_sha256_hex(&"0123456789abcdef".repeat(4)));
        assert!(!is_sha256_hex(&"a".repeat(63)));
        assert!(!is_sha256_hex(&"a".repeat(65)));
        assert!(!is_sha256_hex(&"g".repeat(64)));
        assert!(!is_sha256_hex(""));
    }

    /// The case rule: uppercase hex is not a sha256, and an image digest in it is not canonical;
    /// an image reference spelled in it is read with its digest lowercased.
    #[test]
    fn uppercase_hex_is_not_a_sha256_but_a_typed_reference_is_lowercased() {
        let upper = "A".repeat(64);
        assert!(!is_sha256_hex(&upper));
        assert!(!is_sha256_hex(&"0123456789ABCDEF".repeat(4)));
        assert_eq!(
            format!("sha256:{upper}").parse::<ImageDigest>(),
            Err(ImageDigestError::NonCanonicalHex)
        );

        let image =
            crate::images::ImageRef::parse(&format!("quay.io/x/y:1@sha256:{upper}")).unwrap();
        let digest = image.digest.unwrap();
        assert_eq!(digest, format!("sha256:{}", "a".repeat(64)));
        assert!(digest.parse::<ImageDigest>().is_ok());
    }

    #[test]
    fn a_digest_is_sha256_and_exactly_64_lowercase_hex_characters() {
        // Every hex digit is accepted, in every position.
        let all_digits = "0123456789abcdef".repeat(4);
        for rotation in 0..16 {
            let payload = format!("{}{}", &all_digits[rotation..], &all_digits[..rotation]);
            let value = format!("sha256:{payload}");
            assert_eq!(
                ImageDigest::from_str(&value).map(super::ImageDigest::into_string),
                Ok(value.clone())
            );
        }

        // Hex of any other length is refused, and says how long it was.
        for length in (0..=128).filter(|&n| n != 64) {
            let value = format!("sha256:{}", "0".repeat(length));
            assert_eq!(ImageDigest::from_str(&value), Err(ImageDigestError::WrongLength(length)));
        }

        // Any printable ASCII character that is not a lowercase hex digit, in any position, makes
        // 64 characters non-canonical (every such character at every position).
        let invalid: Vec<char> =
            (' '..='~').filter(|c| !c.is_ascii_digit() && !('a'..='f').contains(c)).collect();
        assert_eq!(invalid.len(), 95 - 16);
        for position in 0..64 {
            for &c in &invalid {
                let mut payload: Vec<char> = all_digits.chars().collect();
                payload[position] = c;
                let value = format!("sha256:{}", payload.into_iter().collect::<String>());
                assert_eq!(
                    ImageDigest::from_str(&value),
                    Err(ImageDigestError::NonCanonicalHex),
                    "{value:?}"
                );
            }
        }

        // The length is counted in bytes: 64 characters with a multibyte one are too long.
        let value = format!("sha256:{}é", "a".repeat(63));
        assert_eq!(ImageDigest::from_str(&value), Err(ImageDigestError::WrongLength(65)));
        assert_eq!(
            ImageDigestError::WrongLength(65).to_string(),
            "sha256 image digest must be 64 hexadecimal characters, found 65 bytes"
        );

        // Only sha256.
        for value in [format!("sha512:{}", "a".repeat(128)), "a".repeat(64)] {
            assert_eq!(ImageDigest::from_str(&value), Err(ImageDigestError::UnsupportedAlgorithm));
        }
    }
}
