//! The deploy's single-job queue-classes (`BIFFIX_SINGLE_JOB_QUEUES`): the queues a single-job run's
//! one Batch job is submitted to, and the exact instance types each one's compute environment runs.

use biffix_core::env::{parse_json, EnvError};

/// The variable the deploy sets them in (`infra/biffix_infra/constructs/control_plane.py`): a JSON
/// array of `{"name": <queue-class>, "queue": <job queue ARN>, "instance_types": [<type>, …]}`, in
/// the deploy config's order (the first is the default). An array, not an object by name, so the
/// order survives every JSON writer.
pub(crate) const VAR: &str = "BIFFIX_SINGLE_JOB_QUEUES";

/// One single-job queue-class of the deploy (one entry of [`VAR`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SingleJobQueue {
    /// The queue-class's name (the deploy config's).
    pub name: String,
    /// Its job queue's ARN.
    pub queue: String,
    /// The exact instance types its compute environment runs, as the deploy config lists them.
    pub instance_types: Vec<String>,
}

/// Parse `BIFFIX_SINGLE_JOB_QUEUES`: unset or blank is none (a deploy without single-job
/// queue-classes). A value that is not such an array, a queue-class listed twice, or one with no
/// name, no queue or no instance type, fails startup naming the variable.
pub(crate) fn parse_single_job_queues(raw: Option<&str>) -> Result<Vec<SingleJobQueue>, EnvError> {
    let expected =
        "[{\"name\": <queue-class>, \"queue\": <job queue ARN>, \"instance_types\": [<type>, …]}, …]";
    let Some(queues) = parse_json::<Vec<SingleJobQueue>>(VAR, raw, expected)? else {
        return Ok(Vec::new());
    };
    for (i, q) in queues.iter().enumerate() {
        if q.name.trim().is_empty() {
            return Err(EnvError::rejected(VAR, format!("entry {i} has no queue-class name")));
        }
        if queues[..i].iter().any(|p| p.name == q.name) {
            return Err(EnvError::rejected(
                VAR,
                format!("queue-class `{}` is listed twice", q.name),
            ));
        }
        if q.queue.trim().is_empty() || q.instance_types.is_empty() {
            return Err(EnvError::rejected(
                VAR,
                format!("queue-class `{}` needs a queue and at least one instance type", q.name),
            ));
        }
    }
    Ok(queues)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_blank_is_none() {
        assert_eq!(parse_single_job_queues(None).unwrap(), vec![]);
        assert_eq!(parse_single_job_queues(Some("  ")).unwrap(), vec![]);
        assert_eq!(parse_single_job_queues(Some("[]")).unwrap(), vec![]);
    }

    #[test]
    fn entries_keep_the_deploy_order() {
        let raw = r#"[{"name":"zeta","queue":"arn:z","instance_types":["c6id.8xlarge"]},
                     {"name":"10","queue":"arn:t","instance_types":["c6id.4xlarge"]},
                     {"name":"alpha","queue":"arn:a","instance_types":["m6id.8xlarge","c6id.8xlarge"]}]"#;
        let queues = parse_single_job_queues(Some(raw)).unwrap();
        assert_eq!(
            queues.iter().map(|q| q.name.as_str()).collect::<Vec<_>>(),
            ["zeta", "10", "alpha"]
        );
        assert_eq!(queues[2].queue, "arn:a");
        assert_eq!(queues[2].instance_types, ["m6id.8xlarge", "c6id.8xlarge"]);
    }

    #[test]
    fn a_malformed_value_fails_naming_the_variable() {
        for raw in [
            "{}",
            r#"{"a":{"queue":"arn:a","instance_types":["x.y"]}}"#,
            r#"[{"name":"a","queue":"arn:a"}]"#,
            r#"[{"queue":"arn:a","instance_types":["x.y"]}]"#,
            r#"[{"name":"","queue":"arn:a","instance_types":["x.y"]}]"#,
            r#"[{"name":"a","queue":"arn:a","instance_types":[]}]"#,
            r#"[{"name":"a","queue":"","instance_types":["c6id.8xlarge"]}]"#,
            r#"[{"name":"a","queue":"q","instance_types":["x.y"],"extra":1}]"#,
            r#"[{"name":"a","queue":"q","instance_types":["x.y"]},{"name":"a","queue":"r","instance_types":["x.y"]}]"#,
        ] {
            let err = parse_single_job_queues(Some(raw)).unwrap_err();
            assert!(err.to_string().contains(VAR), "{raw}: {err}");
        }
    }
}
