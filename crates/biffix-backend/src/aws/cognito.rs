//! Cognito: a submitter's human label, from the user pool (the [`IdentityResolver`] a deploy
//! with Cognito auth uses).

use async_trait::async_trait;

use crate::auth::identity::IdentityResolver;

/// Resolves via Cognito `AdminGetUser` on the user pool. Behind the `aws` feature.
pub(crate) struct CognitoIdentityResolver {
    client: aws_sdk_cognitoidentityprovider::Client,
    user_pool_id: String,
}

impl CognitoIdentityResolver {
    pub(crate) fn new(cfg: &aws_config::SdkConfig, user_pool_id: impl Into<String>) -> Self {
        Self {
            client: aws_sdk_cognitoidentityprovider::Client::new(cfg),
            user_pool_id: user_pool_id.into(),
        }
    }
}

#[async_trait]
impl IdentityResolver for CognitoIdentityResolver {
    async fn label_for(&self, username: &str) -> Option<String> {
        let out = self
            .client
            .admin_get_user()
            .user_pool_id(&self.user_pool_id)
            .username(username)
            .send()
            .await
            // Best-effort (the label is a nicety), but say why it is missing: a missing
            // `cognito-idp:AdminGetUser` grant would otherwise leave every submitter an opaque id.
            .inspect_err(|e| {
                tracing::warn!(
                    error = %aws_sdk_cognitoidentityprovider::error::DisplayErrorContext(e),
                    "identity: could not read the user's label"
                );
            })
            .ok()?;
        // Prefer email, else name. `user_attributes` is a slice of {name, value}.
        let attr = |key: &str| {
            out.user_attributes()
                .iter()
                .find(|a| a.name() == key)
                .and_then(|a| a.value())
                .map(str::to_string)
        };
        attr("email").or_else(|| attr("name"))
    }
}
