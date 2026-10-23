use super::*;
use biffix_core::event::{SampleHint, StepScope};

#[test]
fn a_hint_is_frozen_and_a_tag_without_a_sample_group_is_dropped() {
    let mut form = FormSchema::default();
    attach_samples(&mut form, None);
    assert_eq!(form.samples, None);

    let manifest: crate::manifest::BiffixManifest = serde_norway::from_str(
        "samples:\n  wildcard: donor\n  tag: '^(\\w+)_'\n  steps: {joint: study}\n",
    )
    .unwrap();
    attach_samples(&mut form, manifest.samples.as_ref().map(|s| &s.hint));
    let hint = form.samples.clone().unwrap();
    assert_eq!(hint.wildcard.as_deref(), Some("donor"));
    assert_eq!(hint.tag, None);
    assert_eq!(hint.steps["joint"], StepScope::Study);
    assert!(form.warnings.iter().any(|w| w.contains("samples.tag")), "{:?}", form.warnings);

    let mut form = FormSchema::default();
    let tag_only = SampleHint { tag: Some("^(?P<sample>[^_]+)_".into()), ..Default::default() };
    attach_samples(&mut form, Some(&tag_only));
    assert_eq!(form.samples, Some(tag_only));
}

#[test]
fn an_unknown_step_scope_is_warned_about() {
    let mut form = FormSchema::default();
    let hint = SampleHint {
        steps: [
            ("joint".to_string(), StepScope::Unknown("studdy".into())),
            ("align".to_string(), StepScope::Sample),
        ]
        .into(),
        ..Default::default()
    };
    attach_samples(&mut form, Some(&hint));
    assert_eq!(
        form.warnings,
        ["samples.steps.joint: unknown scope `studdy` (sample or study); ignored"]
    );
}

#[test]
fn a_tag_that_does_not_compile_is_dropped_and_an_emptied_hint_is_none() {
    let mut form = FormSchema::default();
    let bad_only = SampleHint { tag: Some("(?P<sample>[".into()), ..Default::default() };
    attach_samples(&mut form, Some(&bad_only));
    assert_eq!(form.samples, None, "nothing usable is left");
    assert!(form.warnings.iter().any(|w| w.contains("samples.tag")), "{:?}", form.warnings);

    let mut form = FormSchema::default();
    let with_steps = SampleHint {
        tag: Some("(?P<sample>[".into()),
        steps: [("joint".to_string(), StepScope::Study)].into(),
        ..Default::default()
    };
    attach_samples(&mut form, Some(&with_steps));
    assert_eq!(form.samples, Some(SampleHint { tag: None, ..with_steps }));
}
