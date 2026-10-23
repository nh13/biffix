//! How a version is listed in the pipeline catalog (`catalog:` in `biffix.yaml`): its description,
//! tags and contact, normalized and bounded.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::formschema::{is_false, FormSchema};

/// How a version is listed in the Lab's pipeline catalog (`catalog:` in `biffix.yaml`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormCatalog {
    /// A plain-language description for the pipeline's card, trimmed and at most
    /// [`MAX_CATALOG_DESCRIPTION_CHARS`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Tags the catalog filters on: trimmed, non-empty, unique ignoring case (the first spelling
    /// kept), at most [`MAX_CATALOG_TAGS`] of at most [`MAX_CATALOG_TAG_CHARS`] characters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The author vouches for the workflow (a *Verified* badge). Author-declared, not reviewed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub verified: bool,
    /// Who Lab users ask when one of the workflow's runs needs attention; wins over the site's
    /// support contact for its runs. Checked by [`normalize_contact`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<Contact>,
}

/// A person (or team) to ask for help: a workflow's `catalog.contact`, or the site's support
/// contact.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Contact {
    /// Shown as "Ask `<name>`": 1 to [`MAX_CONTACT_NAME_CHARS`] characters.
    pub name: String,
    /// A plausible email address (the *Ask* button's `mailto:`).
    pub email: String,
    /// A help page (a ticket form, a chat channel): an `http(s)` URL of at most
    /// [`MAX_CONTACT_URL_CHARS`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The longest contact name, in characters.
pub const MAX_CONTACT_NAME_CHARS: usize = 100;
/// The longest contact URL, in characters.
pub const MAX_CONTACT_URL_CHARS: usize = 500;

/// A contact with its fields trimmed (a blank URL is none), or the first unusable field and why:
/// `("name", …)` blank or too long, `("email", …)` not a plausible address, `("url", …)` not an
/// `http(s)` URL or too long. The one check for a workflow's `catalog.contact` and the site's
/// support contact.
pub fn normalize_contact(c: &Contact) -> Result<Contact, (&'static str, String)> {
    let name = c.name.trim();
    if name.is_empty() || name.chars().count() > MAX_CONTACT_NAME_CHARS {
        return Err(("name", format!("must be 1 to {MAX_CONTACT_NAME_CHARS} characters")));
    }
    let email = c.email.trim();
    if !crate::text::plausible_email(email) {
        return Err((
            "email",
            format!("`{email}` is not a valid email address ({})", crate::text::EMAIL_RULE),
        ));
    }
    let url = c.url.as_deref().map(str::trim).filter(|u| !u.is_empty());
    if let Some(url) = url {
        if url.chars().count() > MAX_CONTACT_URL_CHARS {
            return Err(("url", format!("must be at most {MAX_CONTACT_URL_CHARS} characters")));
        }
        if !crate::text::is_http_url(url) {
            return Err(("url", "must be an http:// or https:// URL".into()));
        }
    }
    Ok(Contact { name: name.to_string(), email: email.to_string(), url: url.map(str::to_string) })
}

/// The most tags a catalog entry keeps.
pub const MAX_CATALOG_TAGS: usize = 12;
/// The longest tag a catalog entry keeps, in characters.
pub const MAX_CATALOG_TAG_CHARS: usize = 40;
/// The longest catalog description, in characters; a longer one is cut.
pub const MAX_CATALOG_DESCRIPTION_CHARS: usize = 300;

/// Attach the manifest's `catalog:` block to `form`, normalized by [`normalize_catalog`] (its
/// warnings land on the form's). A block that leaves nothing attaches nothing.
pub fn attach_catalog(form: &mut FormSchema, decl: Option<&crate::manifest::CatalogDecl>) {
    let Some(decl) = decl else { return };
    let raw = FormCatalog {
        description: decl.description.clone(),
        tags: decl.tags.clone(),
        verified: decl.verified,
        contact: decl.contact.as_ref().map(|c| Contact {
            name: c.name.clone().unwrap_or_default(),
            email: c.email.clone().unwrap_or_default(),
            url: c.url.clone(),
        }),
    };
    form.catalog = normalize_catalog(&raw, &mut form.warnings);
}

/// How many cut tags a catalog warning names before it only counts the rest.
const MAX_NAMED_IN_WARNING: usize = 5;

/// `items` for a warning: the first [`MAX_NAMED_IN_WARNING`] joined, then how many more.
fn named(items: &[String]) -> String {
    let shown = items[..items.len().min(MAX_NAMED_IN_WARNING)].join(", ");
    match items.len().saturating_sub(MAX_NAMED_IN_WARNING) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

/// A catalog entry as it is frozen: the description trimmed and cut to
/// [`MAX_CATALOG_DESCRIPTION_CHARS`]; tags trimmed, empty ones dropped, de-duplicated ignoring case
/// (keeping the first spelling), and capped at [`MAX_CATALOG_TAGS`] of at most
/// [`MAX_CATALOG_TAG_CHARS`] characters. A cut description, over-long tags and tags past the cap
/// are warned about (into `warnings`); empty and duplicate tags are dropped quietly. A contact is
/// checked by [`normalize_contact`]: one without a usable name or email is dropped, and a bad URL
/// only loses the URL, each with a warning. `None` when nothing is left (no description, no tags,
/// not verified, no contact). Applied both where the CLI derives
/// the form and where the server registers one, so a direct API registration is held to the same
/// limits.
pub fn normalize_catalog(c: &FormCatalog, warnings: &mut Vec<String>) -> Option<FormCatalog> {
    let description = c.description.as_deref().map(str::trim).filter(|d| !d.is_empty()).map(|d| {
        let cut = crate::text::truncate_chars(d, MAX_CATALOG_DESCRIPTION_CHARS);
        if cut.len() < d.len() {
            warnings.push(format!(
                "catalog.description: longer than {MAX_CATALOG_DESCRIPTION_CHARS} characters; \
                     cut"
            ));
        }
        cut.trim_end().to_string()
    });
    let mut seen = std::collections::HashSet::new();
    let mut tags = Vec::new();
    let mut too_long = Vec::new();
    for tag in c.tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        if tag.chars().count() > MAX_CATALOG_TAG_CHARS {
            // Quote only its start: the warning is for a person, not an echo.
            too_long
                .push(format!("`{}…`", crate::text::truncate_chars(tag, MAX_CATALOG_TAG_CHARS)));
        } else if seen.insert(tag.to_lowercase()) {
            tags.push(tag.to_string());
        }
    }
    if !too_long.is_empty() {
        warnings.push(format!(
            "catalog.tags: longer than {MAX_CATALOG_TAG_CHARS} characters; ignored {}",
            named(&too_long)
        ));
    }
    if tags.len() > MAX_CATALOG_TAGS {
        warnings.push(format!(
            "catalog.tags: at most {MAX_CATALOG_TAGS} tags; ignored {}",
            named(&tags[MAX_CATALOG_TAGS..])
        ));
        tags.truncate(MAX_CATALOG_TAGS);
    }
    let contact = c.contact.as_ref().and_then(|raw| match normalize_contact(raw) {
        Ok(contact) => Some(contact),
        Err(("url", why)) => {
            warnings.push(format!("catalog.contact.url: {why}; ignored"));
            normalize_contact(&Contact { url: None, ..raw.clone() }).ok()
        }
        Err((field, why)) => {
            warnings.push(format!("catalog.contact.{field}: {why}; contact ignored"));
            None
        }
    });
    let catalog = FormCatalog { description, tags, verified: c.verified, contact };
    (catalog != FormCatalog::default()).then_some(catalog)
}
