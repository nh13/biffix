//! The ONE way to find the Postgres connection URL, shared by `biffix-backend` and
//! `biffix-ingest`.
//!
//! - [`url_from_secret_json`] (always available, pure) turns an RDS-managed secret into a URL.
//! - `resolve` (behind the opt-in `aws` feature, so the default build pulls no AWS SDK) takes the
//!   URL from a binary's [`biffix_core::env::DbConfig`] (`DATABASE_URL`, else the secret
//!   `BIFFIX_DB_SECRET_ARN` names) or fetches that secret from Secrets Manager.
//!
//! Keep it one resolver: every binary must agree on the database, and the default name (see
//! `default_dbname`) is the part two copies would drift on.

use serde::Deserialize;

/// The subset of the RDS-managed secret JSON that a connection URL needs.
#[derive(Debug, Deserialize)]
struct RdsSecret {
    username: String,
    password: String,
    host: String,
    port: u16,
    /// Absent when the cluster was created without an explicit default database.
    #[serde(default = "default_dbname")]
    dbname: String,
}

/// Aurora Postgres always has `postgres`; a cluster created without `default_database_name` has
/// nothing else. This is where the schema is created, so it must not be changed without migrating.
fn default_dbname() -> String {
    "postgres".to_owned()
}

/// The connection URL could not be built.
#[derive(Debug, thiserror::Error)]
pub enum UrlError {
    /// The database secret could not be read.
    #[cfg(feature = "aws")]
    #[error("database secret: {0}")]
    Secret(#[from] crate::secrets::SecretError),
    /// The secret is not an RDS-managed secret.
    #[error("invalid RDS secret JSON: {0}")]
    Json(#[source] serde_json::Error),
}

/// Resolve the Aurora connection URL `config` names, fetching the secret when it names one.
#[cfg(feature = "aws")]
pub async fn resolve(config: &biffix_core::env::DbConfig) -> Result<String, UrlError> {
    let arn = match config {
        biffix_core::env::DbConfig::Url(url) => return Ok(url.clone()),
        biffix_core::env::DbConfig::Secret(arn) => arn,
    };
    let sdk = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let value = crate::secrets::fetch_secret_string(&sdk, arn).await?;
    url_from_secret_json(&value)
}

/// Build a percent-encoded PostgreSQL URL from an RDS-managed secret value.
pub fn url_from_secret_json(value: &str) -> Result<String, UrlError> {
    let secret: RdsSecret = serde_json::from_str(value).map_err(UrlError::Json)?;
    // Percent-encode the credentials and the database name: a generated password may contain
    // URL-hostile characters, and silently producing a malformed URL (or one naming another
    // database) is worse than failing. sqlx decodes each again.
    let username = urlencoding::encode(&secret.username);
    let password = urlencoding::encode(&secret.password);
    let dbname = urlencoding::encode(&secret.dbname);
    Ok(format!("postgresql://{username}:{password}@{}:{}/{dbname}", secret.host, secret.port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_exact_url() {
        let json = r#"{"username":"biffix","password":"pw","host":"db.internal","port":5433,"dbname":"biffix"}"#;
        assert_eq!(
            url_from_secret_json(json).unwrap(),
            "postgresql://biffix:pw@db.internal:5433/biffix"
        );
    }

    #[test]
    fn percent_encodes_username_and_password() {
        // Every URL-hostile character in either credential: `@` and `:` would split the authority,
        // `/`, `?` and `#` would end it, `%` would start an escape, and a space is not allowed.
        let json = r#"{"username":"a@b:c d","password":"p@ss:/w?r#d%1 x","host":"h","port":5432}"#;
        assert_eq!(
            url_from_secret_json(json).unwrap(),
            "postgresql://a%40b%3Ac%20d:p%40ss%3A%2Fw%3Fr%23d%251%20x@h:5432/postgres"
        );
    }

    #[test]
    fn percent_encodes_the_database_name() {
        // `/`, `?` and `#` in the path would name another database or start the query string.
        let json = r#"{"username":"u","password":"p","host":"h","port":5432,"dbname":"a/b?c#d e"}"#;
        assert_eq!(
            url_from_secret_json(json).unwrap(),
            "postgresql://u:p@h:5432/a%2Fb%3Fc%23d%20e"
        );
    }

    #[test]
    fn defaults_dbname_to_postgres() {
        let json = r#"{"username":"u","password":"p","host":"h","port":5432}"#;
        assert_eq!(url_from_secret_json(json).unwrap(), "postgresql://u:p@h:5432/postgres");
    }

    #[test]
    fn rejects_a_malformed_or_incomplete_secret() {
        assert!(url_from_secret_json("{not json}").is_err());
        assert!(url_from_secret_json(r#"{"username":"u","password":"p","host":"h"}"#).is_err());
        assert!(url_from_secret_json(r#"{"username":"u","password":"p","host":"h","port":"x"}"#)
            .is_err());
    }
}
