//! [`Secret`]: a value whose `Debug`/`Display` render `[redacted]`, so a token, code, verifier or
//! refresh token cannot reach a log line or an error message through ordinary formatting.

use std::fmt;

/// A redacting wrapper. The inner value is private: the only way to read it is [`Secret::expose`],
/// so every place a secret leaves the wrapper is a named, greppable call.
///
/// Serde is deliberately **transparent**: the only place a secret is written is the mode-`0600`
/// credential file, which is the secure store itself. Redaction protects logs and error output,
/// not the store the OS already guards.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Secret(value)
    }

    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_renders_its_value() {
        let s = Secret::new("super-secret-token".to_string());
        assert_eq!(format!("{s}"), "[redacted]");
        assert_eq!(format!("{s:?}"), "[redacted]");
        assert_eq!(s.expose(), "super-secret-token");
    }

    #[test]
    fn serde_is_transparent() {
        // The credential file must hold the real value, not `[redacted]`.
        let s = Secret::new("tok".to_string());
        assert_eq!(serde_json::to_string(&s).unwrap(), "\"tok\"");
        let back: Secret<String> = serde_json::from_str("\"tok\"").unwrap();
        assert_eq!(back.expose(), "tok");
    }
}
