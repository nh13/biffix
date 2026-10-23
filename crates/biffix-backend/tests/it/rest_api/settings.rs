//! Site settings: logo, title and support contact.

use super::*;

const LOGO: &str = "data:image/png;base64,iVBORw0KGgo=";

#[tokio::test]
async fn settings_round_trip_and_an_omitted_field_is_kept() {
    let app = app_with(MemStore::new());
    let (status, v) = call(
        app.clone(),
        Method::PUT,
        "/settings",
        Some(json!({"title": "  Genomics Lab ", "logo_data_url": LOGO})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"title": "Genomics Lab", "logo_data_url": LOGO}));
    let (_, v) = get_json(app.clone(), "/settings").await;
    assert_eq!(v, json!({"title": "Genomics Lab", "logo_data_url": LOGO}));

    // A title-only update (a script, the CLI) leaves the logo alone.
    let (_, v) = call(app.clone(), Method::PUT, "/settings", Some(json!({"title": "Lab"}))).await;
    assert_eq!(v, json!({"title": "Lab", "logo_data_url": LOGO}));
    let (_, v) = get_json(app.clone(), "/settings").await;
    assert_eq!(v["logo_data_url"], LOGO);

    // `null`, an empty logo and a blank title each clear their field.
    let (_, v) =
        call(app.clone(), Method::PUT, "/settings", Some(json!({"logo_data_url": null}))).await;
    assert_eq!(v, json!({"title": "Lab"}));
    let (_, v) = call(
        app.clone(),
        Method::PUT,
        "/settings",
        Some(json!({"title": "  ", "logo_data_url": ""})),
    )
    .await;
    assert_eq!(v, json!({}));
    let (_, v) = get_json(app, "/settings").await;
    assert_eq!(v, json!({}));
}

#[tokio::test]
async fn settings_refuse_a_bad_logo_or_title_and_keep_what_was_stored() {
    let app = app_with(MemStore::new());
    call(
        app.clone(),
        Method::PUT,
        "/settings",
        Some(json!({"title": "Lab", "logo_data_url": LOGO})),
    )
    .await;
    let huge = format!("data:image/png;base64,{}", "A".repeat(512 * 1024));
    for (body, why) in [
        (json!({"logo_data_url": "https://example.org/logo.png"}), "data: URL"),
        (json!({"logo_data_url": "data:text/html;base64,PHNjcmlwdD4="}), "must be one of"),
        (json!({"logo_data_url": "data:image/svg+xml;base64,PHN2Zz4="}), "must be one of"),
        (json!({"logo_data_url": huge}), "too large"),
        (json!({"title": "x".repeat(101)}), "at most 100"),
    ] {
        let (status, v) = call(app.clone(), Method::PUT, "/settings", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
        assert!(v.as_str().unwrap().contains(why), "{why}: {v}");
    }
    let (_, v) = get_json(app.clone(), "/settings").await;
    assert_eq!(v, json!({"title": "Lab", "logo_data_url": LOGO}));

    // The bound is 100 characters, not bytes: 100 two-byte characters are accepted.
    let longest = "é".repeat(100);
    let (status, v) = call(app, Method::PUT, "/settings", Some(json!({"title": longest}))).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["title"], json!(longest));
}

#[tokio::test]
async fn a_support_contact_is_trimmed_kept_by_other_updates_and_cleared_by_null() {
    let app = app_with(MemStore::new());
    let (status, v) = call(
        app.clone(),
        Method::PUT,
        "/settings",
        Some(
            json!({"support": {"name": " Bioinformatics core ", "email": " core@lab.example.org ",
                                "url": " https://lab.example.org/help "}}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let stored = json!({"name": "Bioinformatics core", "email": "core@lab.example.org",
                        "url": "https://lab.example.org/help"});
    assert_eq!(v, json!({"support": stored}));
    // A title-only update leaves it alone.
    let (_, v) = call(app.clone(), Method::PUT, "/settings", Some(json!({"title": "Lab"}))).await;
    assert_eq!(v, json!({"title": "Lab", "support": stored}));
    let (_, v) = get_json(app.clone(), "/settings").await;
    assert_eq!(v["support"], stored);
    // A blank url is none.
    let (_, v) = call(
        app.clone(),
        Method::PUT,
        "/settings",
        Some(json!({"support": {"name": "Core", "email": "core@lab.example.org", "url": ""}})),
    )
    .await;
    assert_eq!(v["support"], json!({"name": "Core", "email": "core@lab.example.org"}));
    let (_, v) = call(app.clone(), Method::PUT, "/settings", Some(json!({"support": null}))).await;
    assert_eq!(v, json!({"title": "Lab"}));
}

#[tokio::test]
async fn a_bad_support_contact_is_refused_naming_the_field() {
    let app = app_with(MemStore::new());
    let ok = json!({"name": "Core", "email": "core@lab.example.org"});
    call(app.clone(), Method::PUT, "/settings", Some(json!({"support": ok}))).await;
    for (support, why) in [
        (json!({"name": "  ", "email": "core@lab.example.org"}), "support.name"),
        (json!({"name": "x".repeat(101), "email": "core@lab.example.org"}), "support.name"),
        (json!({"name": "Core", "email": "core"}), "support.email"),
        (
            json!({"name": "Core", "email": "core@lab.example.org", "url": "javascript:alert(1)"}),
            "support.url",
        ),
        (
            json!({"name": "Core", "email": "core@lab.example.org",
                   "url": format!("https://lab.example.org/{}", "a".repeat(500))}),
            "support.url",
        ),
    ] {
        let (status, v) =
            call(app.clone(), Method::PUT, "/settings", Some(json!({"support": support}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
        assert!(v.as_str().unwrap().contains(why), "{why}: {v}");
    }
    let (_, v) = get_json(app, "/settings").await;
    assert_eq!(v["support"], ok);
}
