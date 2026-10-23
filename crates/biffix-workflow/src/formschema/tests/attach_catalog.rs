use super::*;
use crate::manifest::{BiffixManifest, CatalogDecl};

fn catalog_of(yaml: &str) -> FormSchema {
    let manifest = BiffixManifest::parse_yaml(yaml).unwrap();
    let mut form = FormSchema::default();
    attach_catalog(&mut form, manifest.catalog.as_ref());
    form
}

#[test]
fn a_catalog_block_is_normalized_and_frozen() {
    let form = catalog_of(
        "catalog:\n  description: '  Bulk RNA-seq, reads to counts.  '\n  \
             tags: [' RNA-seq ', Bulk, '', rna-SEQ, '  ']\n  verified: true\n",
    );
    assert_eq!(
        form.catalog,
        Some(FormCatalog {
            description: Some("Bulk RNA-seq, reads to counts.".into()),
            tags: vec!["RNA-seq".into(), "Bulk".into()],
            verified: true,
            contact: None,
        })
    );
    // Empty and duplicate tags are dropped quietly: only cuts are warned about.
    assert!(form.warnings.is_empty(), "{:?}", form.warnings);
    // Round-trips through the frozen JSON.
    let json = serde_json::to_value(&form).unwrap();
    assert_eq!(json["catalog"]["tags"][1], "Bulk");
    let back: FormSchema = serde_json::from_value(json).unwrap();
    assert_eq!(back, form);
}

#[test]
fn no_block_or_an_empty_one_attaches_nothing() {
    assert_eq!(catalog_of("engine: snakemake\n").catalog, None);
    assert_eq!(catalog_of("catalog: {}\n").catalog, None);
    let form = catalog_of("catalog: { description: '  ', tags: ['', ' '], verified: false }\n");
    assert_eq!(form.catalog, None);
    assert!(form.warnings.is_empty(), "{:?}", form.warnings);
    // Verified alone is something to show.
    assert_eq!(
        catalog_of("catalog: { verified: true }\n").catalog,
        Some(FormCatalog { verified: true, ..Default::default() })
    );
    // A form frozen before catalogs existed reads as none.
    let old: FormSchema =
        serde_json::from_value(serde_json::json!({"fields": [], "warnings": []})).unwrap();
    assert_eq!(old.catalog, None);
}

#[test]
fn oversized_tags_extra_tags_and_a_long_description_are_cut_with_warnings() {
    let long_tag = "x".repeat(MAX_CATALOG_TAG_CHARS + 1);
    let exact_tag = "é".repeat(MAX_CATALOG_TAG_CHARS);
    let mut tags = vec![long_tag.clone(), exact_tag.clone()];
    tags.extend((0..MAX_CATALOG_TAGS + 2).map(|i| format!("t{i}")));
    let decl = CatalogDecl {
        description: Some(format!("{} tail", "ü".repeat(MAX_CATALOG_DESCRIPTION_CHARS - 1))),
        tags,
        ..Default::default()
    };
    let mut form = FormSchema::default();
    attach_catalog(&mut form, Some(&decl));
    let catalog = form.catalog.clone().unwrap();
    // Cut at the cap, then trimmed: the space before `tail` falls at the boundary.
    assert_eq!(
        catalog.description.as_deref(),
        Some("ü".repeat(MAX_CATALOG_DESCRIPTION_CHARS - 1).as_str())
    );
    assert_eq!(catalog.tags.len(), MAX_CATALOG_TAGS);
    assert_eq!(catalog.tags[0], exact_tag);
    assert_eq!(catalog.tags[MAX_CATALOG_TAGS - 1], "t10");
    assert!(!catalog.verified);
    assert_eq!(
        form.warnings,
        [
            format!(
                "catalog.description: longer than {MAX_CATALOG_DESCRIPTION_CHARS} characters; cut"
            ),
            format!(
                "catalog.tags: longer than {MAX_CATALOG_TAG_CHARS} characters; ignored `{}…`",
                &long_tag[..MAX_CATALOG_TAG_CHARS]
            ),
            format!("catalog.tags: at most {MAX_CATALOG_TAGS} tags; ignored t11, t12, t13"),
        ]
    );
}

#[test]
fn many_tags_are_capped_and_the_warning_names_only_a_few() {
    let raw = FormCatalog {
        description: Some("d".repeat(5000)),
        tags: (0..1000).map(|i| format!("t{i}")).collect(),
        ..Default::default()
    };
    let mut warnings = Vec::new();
    let catalog = normalize_catalog(&raw, &mut warnings).unwrap();
    assert_eq!(catalog.tags.len(), MAX_CATALOG_TAGS);
    assert_eq!(
        catalog.description.as_deref().map(|d| d.chars().count()),
        Some(MAX_CATALOG_DESCRIPTION_CHARS)
    );
    assert_eq!(
        warnings[1],
        format!(
            "catalog.tags: at most {MAX_CATALOG_TAGS} tags; ignored t12, t13, t14, t15, t16 \
                 and 983 more"
        )
    );
    // Nothing left: nothing to freeze.
    let mut warnings = Vec::new();
    assert_eq!(normalize_catalog(&FormCatalog::default(), &mut warnings), None);
    assert!(warnings.is_empty());
}

#[test]
fn a_contact_is_trimmed_and_frozen() {
    let form = catalog_of(
        "catalog:\n  contact: { name: ' Genomics core ', email: ' core@lab.example.org ', \
             url: 'https://lab.example.org/help ' }\n",
    );
    assert_eq!(
        form.catalog.and_then(|c| c.contact),
        Some(Contact {
            name: "Genomics core".into(),
            email: "core@lab.example.org".into(),
            url: Some("https://lab.example.org/help".into()),
        })
    );
    assert!(form.warnings.is_empty(), "{:?}", form.warnings);
    // A contact alone is something to show.
    let form = catalog_of("catalog: { contact: { name: Core, email: core@lab.example.org } }\n");
    assert_eq!(form.catalog.and_then(|c| c.contact).map(|c| c.url), Some(None));
}

#[test]
fn an_unusable_contact_is_dropped_and_a_bad_url_only_loses_the_url() {
    for (yaml, warning) in [
        (
            "catalog: { tags: [x], contact: { name: '  ', email: core@lab.example.org } }\n",
            "catalog.contact.name: must be 1 to 100 characters; contact ignored",
        ),
        (
            "catalog: { tags: [x], contact: { email: core@lab.example.org } }\n",
            "catalog.contact.name: must be 1 to 100 characters; contact ignored",
        ),
        (
            "catalog: { tags: [x], contact: { name: Core, email: 'core at lab' } }\n",
            "catalog.contact.email: `core at lab` is not a valid email address (one `@`, a \
             dotted domain, no spaces, and none of `? & # % , < > =` (unsafe in a mail link)); \
             contact ignored",
        ),
    ] {
        let form = catalog_of(yaml);
        assert_eq!(form.catalog.unwrap().contact, None, "{yaml}");
        assert_eq!(form.warnings, [warning], "{yaml}");
    }
    let form = catalog_of(
        "catalog: { contact: { name: Core, email: core@lab.example.org, url: 'javascript:x' } }\n",
    );
    assert_eq!(
        form.catalog.and_then(|c| c.contact),
        Some(Contact { name: "Core".into(), email: "core@lab.example.org".into(), url: None })
    );
    assert_eq!(form.warnings, ["catalog.contact.url: must be an http:// or https:// URL; ignored"]);
}

#[test]
fn a_contact_is_checked_field_by_field() {
    let ok = Contact {
        name: "Core".into(),
        email: "core@lab.example.org".into(),
        url: Some("  ".into()),
    };
    // A blank URL is none.
    assert_eq!(normalize_contact(&ok).unwrap().url, None);
    let long_name = Contact { name: "n".repeat(MAX_CONTACT_NAME_CHARS + 1), ..ok.clone() };
    assert_eq!(normalize_contact(&long_name).unwrap_err().0, "name");
    let long_url = Contact {
        url: Some(format!("https://lab.example.org/{}", "a".repeat(MAX_CONTACT_URL_CHARS))),
        ..ok.clone()
    };
    assert_eq!(normalize_contact(&long_url).unwrap_err().0, "url");
}
