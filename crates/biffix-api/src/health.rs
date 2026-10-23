//! `health` — the service-status report.

use serde::{Deserialize, Serialize};

/// Declares [`HealthStatus`] from ONE `Variant => "wire"` table: each variant's serde name,
/// [`HealthStatus::ALL`], [`HealthStatus::as_str`] (and so `Display` and [`HealthStatus::parse`])
/// all expand from it, so the wire value and the stored label cannot drift apart.
///
/// The wire strings are `tt`, not `literal`: a `literal` fragment reaches the derives wrapped in an
/// invisible group, which serde reads but utoipa's `rename` parser silently ignores (it would
/// publish the Rust variant names in the OpenAPI enum).
macro_rules! status_tiers {
    ($(#[$meta:meta])* $($variant:ident => $wire:tt),+ $(,)?) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
        pub enum HealthStatus {
            $(
                #[serde(rename = $wire)]
                $variant,
            )+
        }

        impl HealthStatus {
            /// Every tier, best to worst.
            pub const ALL: &'static [HealthStatus] = &[$(HealthStatus::$variant),+];

            /// The lowercase wire label (the serde representation).
            pub fn as_str(self) -> &'static str {
                match self {
                    $(HealthStatus::$variant => $wire,)+
                }
            }
        }
    };
}

status_tiers! {
    /// A component's health tier. Explicit (not a `healthy: bool`), so a healthy-but-noisy component can
    /// carry a `detail` without being read as degraded.
    ///
    /// Variants are declared worst-last, so the derived `Ord` *is* the severity ordering — rolling
    /// components up to an overall tier is [`HealthStatus::worse`] (a `max`).
    Ok => "ok",
    Degraded => "degraded",
    Down => "down",
}

impl HealthStatus {
    /// The worse of two tiers (for rolling components up to an overall status).
    pub fn worse(self, other: HealthStatus) -> HealthStatus {
        self.max(other)
    }

    /// Parse a stored tier string, defaulting an unknown value to `Degraded`: a sample this build
    /// cannot read (a tier a newer probe added, a spelling that drifted) is no evidence of health,
    /// so the status page must not fail open and show it green.
    pub fn parse(s: &str) -> HealthStatus {
        HealthStatus::ALL
            .iter()
            .copied()
            .find(|tier| tier.as_str() == s)
            .unwrap_or(HealthStatus::Degraded)
    }
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One component's health within a [`StatusReport`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ComponentHealth {
    pub name: String,
    pub status: HealthStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `GET /status` — the worst tier across `components`, plus each component's own tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StatusReport {
    /// Worst of the components.
    pub status: HealthStatus,
    pub components: Vec<ComponentHealth>,
}

/// One bar of a component's uptime history: how many probe samples in the bucket saw each tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UptimeBucket {
    /// Bucket start as epoch seconds (aligned to the bucket interval).
    pub t: i64,
    pub ok: i64,
    pub degraded: i64,
    pub down: i64,
}

/// One component's uptime history within a [`StatusHistory`], buckets in time order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ComponentHistory {
    pub name: String,
    pub buckets: Vec<UptimeBucket>,
}

/// `GET /status/history` — bucketed uptime history for the status page's bars.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StatusHistory {
    /// The bucket width in seconds (the client builds a fixed timeline from this + `window_secs`).
    pub bucket_secs: i64,
    /// The full window span in seconds.
    pub window_secs: i64,
    /// Samples expected per full bucket at the probe cadence (a bucket with fewer is a partial/data
    /// gap, drawn grey rather than a false green).
    pub expected_per_bucket: i64,
    pub components: Vec<ComponentHistory>,
}

/// `GET /status/history` query: which window to bucket.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct StatusHistoryQuery {
    /// `1h`, `1d`, `1w` or `90d` (default): selects both the span and the bucket size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_labels_match_serde_and_parse_back() {
        for &tier in HealthStatus::ALL {
            let wire = serde_json::to_value(tier).unwrap();
            assert_eq!(wire.as_str().unwrap(), tier.as_str());
            assert_eq!(serde_json::from_value::<HealthStatus>(wire).unwrap(), tier);
            assert_eq!(HealthStatus::parse(tier.as_str()), tier);
            assert_eq!(tier.to_string(), tier.as_str());
        }
    }

    #[test]
    fn status_parse_defaults_unknown_to_degraded() {
        assert_eq!(HealthStatus::parse("Down"), HealthStatus::Degraded);
        assert_eq!(HealthStatus::parse(""), HealthStatus::Degraded);
    }

    #[test]
    fn status_all_is_in_severity_order() {
        assert!(HealthStatus::ALL.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(HealthStatus::ALL.first(), Some(&HealthStatus::Ok));
    }
}
