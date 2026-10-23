//! Secret-parameter redaction at control-plane serialization boundaries.
//!
//! Raw values are needed transiently for validation and workflow launch, but only this redacted
//! snapshot may be handed to `run_submitted`. Audit, provenance, and UI views all consume that
//! immutable event, so redacting before event construction keeps the secret out of every
//! downstream representation by construction.

use biffix_workflow::formschema::FormField;
use serde_json::Value;

use biffix_workflow::formschema::REDACTED_SECRET_PLACEHOLDER;

/// Consume a resolved parameter object and replace every present secret-marked field.
///
/// Missing optional secrets remain missing. Every present value is replaced regardless of its
/// JSON type, including `null`, so no submitted representation can cross the event boundary.
/// Non-object values are returned unchanged defensively; the submit pipeline rejects them during
/// schema validation before this function is called.
pub(crate) fn redact_secret_params(fields: &[FormField], mut params: Value) -> Value {
    let Some(object) = params.as_object_mut() else {
        return params;
    };

    for field in fields.iter().filter(|field| field.is_secret) {
        if let Some(value) = object.get_mut(&field.name) {
            *value = Value::String(REDACTED_SECRET_PLACEHOLDER.to_owned());
        }
    }

    params
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, is_secret: bool) -> FormField {
        FormField {
            name: name.to_owned(),
            widget: biffix_workflow::formschema::WidgetHint::Text,
            required: false,
            description: None,
            enum_values: None,
            is_output: false,
            is_secret,
            is_path_input: false,
            ..Default::default()
        }
    }

    #[test]
    fn replaces_present_secrets_and_preserves_public_values() {
        let params = serde_json::json!({
            "api_key": "credential-value",
            "mode": "fast",
            "nested_public": {"keep": [1, 2, 3]}
        });

        let redacted =
            redact_secret_params(&[field("api_key", true), field("mode", false)], params);

        assert_eq!(
            redacted,
            serde_json::json!({
                "api_key": REDACTED_SECRET_PLACEHOLDER,
                "mode": "fast",
                "nested_public": {"keep": [1, 2, 3]}
            })
        );
    }

    #[test]
    fn does_not_insert_an_absent_optional_secret() {
        let redacted = redact_secret_params(
            &[field("optional_token", true)],
            serde_json::json!({"mode": "fast"}),
        );

        assert_eq!(redacted, serde_json::json!({"mode": "fast"}));
    }

    #[test]
    fn replaces_every_json_shape_with_the_same_placeholder() {
        let submitted_values = [
            Value::Null,
            Value::Bool(true),
            serde_json::json!(42),
            serde_json::json!("credential-value"),
            serde_json::json!(["credential-value", {"nested": "secret"}]),
            serde_json::json!({"nested": ["secret"]}),
        ];

        for submitted in submitted_values {
            let redacted = redact_secret_params(
                &[field("secret", true)],
                serde_json::json!({"secret": submitted}),
            );
            assert_eq!(
                redacted.get("secret"),
                Some(&Value::String(REDACTED_SECRET_PLACEHOLDER.to_owned()))
            );
        }
    }
}
