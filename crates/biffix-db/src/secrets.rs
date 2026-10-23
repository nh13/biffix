//! Read a Secrets Manager secret (feature `aws`): the one implementation behind the database
//! secret ([`crate::url::resolve`]) and the backend's capability signing key.

/// A secret that could not be read.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// Secrets Manager refused or failed the read (`message` is the SDK error with its causes).
    #[error("failed to read secret {arn}: {message}")]
    Read { arn: String, message: String },
    /// The secret holds a binary value, not a `SecretString`.
    #[error("secret {arn} has no SecretString")]
    NoSecretString { arn: String },
}

/// Read a Secrets Manager secret's plaintext `SecretString`.
pub async fn fetch_secret_string(
    config: &aws_config::SdkConfig,
    arn: &str,
) -> Result<String, SecretError> {
    let output = aws_sdk_secretsmanager::Client::new(config)
        .get_secret_value()
        .secret_id(arn)
        .send()
        .await
        .map_err(|error| SecretError::Read {
            arn: arn.to_owned(),
            message: aws_sdk_secretsmanager::error::DisplayErrorContext(&error).to_string(),
        })?;
    output
        .secret_string()
        .map(str::to_owned)
        .ok_or_else(|| SecretError::NoSecretString { arn: arn.to_owned() })
}
