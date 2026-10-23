//! A host's spot/on-demand label from its job queue's configured capacity (`BIFFIX_QUEUE_CAPACITIES`),
//! the fallback when EC2 never reported the instance's own lifecycle.

use biffix_api::Host;
use biffix_core::env::{parse_json, EnvError};

/// The variable the deploy sets the capacities in.
pub(crate) const VAR: &str = "BIFFIX_QUEUE_CAPACITIES";

/// `BIFFIX_QUEUE_CAPACITIES`: the physical queue-name prefix once, then each capacity's
/// queue-classes (`{"prefix":"biffix-t-","SPOT":["cpu-spot"],"ON_DEMAND":["coordinator"]}`), so the
/// variable stays small inside Lambda's 4 KB environment (infra api_stack.py).
#[derive(serde::Deserialize)]
struct QueueCapacities {
    prefix: String,
    #[serde(flatten)]
    classes: std::collections::HashMap<String, Vec<String>>,
}

/// Parse `BIFFIX_QUEUE_CAPACITIES` (see [`QueueCapacities`]) into physical queue name (prefix +
/// class) → `spot`/`on_demand`. An empty (or all-whitespace) value is an empty map, since the
/// variable is optional. A non-empty value that is not such an object is a deploy error and fails
/// startup naming the variable, rather than silently dropping every host's capacity label.
pub(crate) fn parse_queue_capacities(
    json: &str,
) -> Result<std::collections::HashMap<String, String>, EnvError> {
    let expected = "{\"prefix\": …, <capacity>: [queue-class, …]}";
    let Some(parsed) = parse_json::<QueueCapacities>(VAR, Some(json), expected)? else {
        return Ok(Default::default());
    };
    Ok(parsed
        .classes
        .into_iter()
        .flat_map(|(capacity, classes)| {
            let capacity = capacity.to_ascii_lowercase();
            let prefix = parsed.prefix.clone();
            classes.into_iter().map(move |class| (format!("{prefix}{class}"), capacity.clone()))
        })
        .collect())
}

/// When EC2 never reported a host's own lifecycle, label it with its job queue's configured
/// capacity instead (`capacity_source: queue`). A queue since removed from the deploy stays
/// unlabeled.
pub(crate) fn fill_queue_capacity(
    host: &mut Host,
    queue_capacities: &std::collections::HashMap<String, String>,
) {
    if host.capacity.is_some() {
        return;
    }
    if let Some(capacity) = host.queue.as_ref().and_then(|q| queue_capacities.get(q)) {
        host.capacity = Some(capacity.clone());
        host.capacity_source = Some("queue".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_capacities_parse_lowercases_and_rejects_garbage() {
        let caps = parse_queue_capacities(
            r#"{"prefix":"biffix-t-","SPOT":["cpu-spot","arch-a"],"ON_DEMAND":["coordinator"]}"#,
        )
        .unwrap();
        assert_eq!(
            caps,
            [
                ("biffix-t-cpu-spot", "spot"),
                ("biffix-t-arch-a", "spot"),
                ("biffix-t-coordinator", "on_demand"),
            ]
            .into_iter()
            .map(|(q, c)| (q.to_string(), c.to_string()))
            .collect()
        );
        assert!(parse_queue_capacities("").unwrap().is_empty());
        assert!(parse_queue_capacities("  ").unwrap().is_empty());
        let err = parse_queue_capacities("not json").unwrap_err().to_string();
        assert!(err.contains("BIFFIX_QUEUE_CAPACITIES"), "{err}");
        // The old shape (queue name -> capacity) and a missing prefix are refused, not misread.
        assert!(parse_queue_capacities(r#"{"q-spot":"SPOT"}"#).is_err());
        assert!(parse_queue_capacities(r#"{"SPOT":["q"]}"#).is_err());
        assert!(parse_queue_capacities(r#"{"prefix":"p-","SPOT":[1]}"#).is_err());
    }

    #[test]
    fn queue_capacity_fills_only_an_unknown_lifecycle() {
        let caps = parse_queue_capacities(r#"{"prefix":"q-","SPOT":["spot"]}"#).unwrap();
        let mut from_queue = Host { queue: Some("q-spot".into()), ..Default::default() };
        fill_queue_capacity(&mut from_queue, &caps);
        assert_eq!(from_queue.capacity.as_deref(), Some("spot"));
        assert_eq!(from_queue.capacity_source.as_deref(), Some("queue"));

        // EC2's own answer wins over the queue's configuration.
        let mut from_instance = Host {
            queue: Some("q-spot".into()),
            capacity: Some("on_demand".into()),
            capacity_source: Some("instance".into()),
            ..Default::default()
        };
        fill_queue_capacity(&mut from_instance, &caps);
        assert_eq!(from_instance.capacity.as_deref(), Some("on_demand"));
        assert_eq!(from_instance.capacity_source.as_deref(), Some("instance"));

        // An unknown queue stays unlabeled.
        let mut unknown = Host { queue: Some("gone".into()), ..Default::default() };
        fill_queue_capacity(&mut unknown, &caps);
        assert_eq!(unknown.capacity, None);
    }
}
