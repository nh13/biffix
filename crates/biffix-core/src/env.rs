//! Process environment configuration, parsed one way by every binary.
//!
//! Each binary reads its environment once, at startup, into its own typed config (the backend's
//! `config::env`, the ingest crate's `config`) through an [`Env`], and passes that config down; no
//! other code reads the environment. The shapes are parsed alike everywhere:
//!
//! - a value is trimmed, and an optional variable that is unset or blank is absent;
//! - a list is comma-separated, its entries trimmed and empty ones dropped (`a, b,,c` → `[a, b, c]`);
//! - a boolean is `true` or `false` (blank is unset);
//! - a number is a non-negative integer; a JSON value is parsed into its type;
//! - a malformed value is an [`EnvError`] naming the variable and the shape it expects.
//!
//! The variable names (and the values they accept) are an operator-facing contract the deploy's CDK
//! stacks set, so a config keeps every name and every accepted value; [`Env::read`] lists the names
//! a config read, for the tests that hold each binary to what the deploy sets.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

/// A boxed parse failure: what a malformed value failed with.
pub type ParseFailure = Box<dyn std::error::Error + Send + Sync>;

/// A variable that is missing or malformed. Every variant names the variable.
#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    /// A required variable is unset (or blank).
    #[error("missing required env var {var}: the deploy must set it")]
    Missing { var: String },
    /// None of the variables one of which is required is set.
    #[error("missing required env var: set {} ({why})", vars.join(" or "))]
    MissingOneOf { vars: Vec<String>, why: String },
    /// A value that is not of the variable's shape.
    #[error("invalid {var}={value:?}: expected {expected} ({source})")]
    Invalid {
        var: String,
        value: String,
        expected: &'static str,
        #[source]
        source: ParseFailure,
    },
    /// A value of the right shape that the config refuses.
    #[error("{var}: {reason}")]
    Rejected { var: String, reason: String },
}

impl EnvError {
    /// The variable the error is about (the first, for [`EnvError::MissingOneOf`]).
    pub fn var(&self) -> &str {
        match self {
            Self::Missing { var } | Self::Invalid { var, .. } | Self::Rejected { var, .. } => var,
            Self::MissingOneOf { vars, .. } => vars.first().map_or("", String::as_str),
        }
    }

    /// `var` refused for `reason`.
    pub fn rejected(var: &str, reason: impl Into<String>) -> Self {
        Self::Rejected { var: var.to_owned(), reason: reason.into() }
    }

    /// `var`'s `value` is not `expected`: it failed with `source`.
    pub fn invalid(
        var: &str,
        value: &str,
        expected: &'static str,
        source: impl Into<ParseFailure>,
    ) -> Self {
        Self::Invalid {
            var: var.to_owned(),
            value: value.to_owned(),
            expected,
            source: source.into(),
        }
    }
}

/// Every error a config found, so one failed start reports all of them rather than the first.
#[derive(Debug)]
pub struct ConfigErrors<E>(pub Vec<E>);

impl<E: std::fmt::Display> std::fmt::Display for ConfigErrors<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let errors: Vec<String> = self.0.iter().map(ToString::to_string).collect();
        write!(f, "invalid configuration: {}", errors.join("; "))
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for ConfigErrors<E> {}

/// Collects a config's errors while it goes on reading: [`Errors::take`] keeps a failed read's
/// error and gives the caller a placeholder, and [`Errors::finish`] fails if any read failed.
#[derive(Debug)]
pub struct Errors<E>(Vec<E>);

impl<E> Default for Errors<E> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<E> Errors<E> {
    /// The value of `result`, or (keeping its error) the type's default.
    pub fn take<T: Default>(&mut self, result: Result<T, impl Into<E>>) -> T {
        result.unwrap_or_else(|error| {
            self.0.push(error.into());
            T::default()
        })
    }

    /// Keep `error`.
    pub fn push(&mut self, error: impl Into<E>) {
        self.0.push(error.into());
    }

    /// `value` when nothing failed, else every error.
    pub fn finish<T>(self, value: T) -> Result<T, ConfigErrors<E>> {
        if self.0.is_empty() {
            Ok(value)
        } else {
            Err(ConfigErrors(self.0))
        }
    }
}

/// A snapshot of the environment a config is read from, recording which variables it read.
#[derive(Debug, Default)]
pub struct Env {
    vars: BTreeMap<String, String>,
    /// The variables set to a value that is not UTF-8: unset to every reader, but a required one
    /// is reported as invalid rather than missing.
    not_utf8: BTreeSet<String>,
    read: RefCell<BTreeSet<String>>,
}

impl Env {
    /// The process environment. A variable whose value is not UTF-8 reads as unset, except that
    /// [`Env::required`] and [`Env::present`] report it as invalid; one whose name is not UTF-8 is
    /// never read.
    pub fn process() -> Self {
        Self::from_os_pairs(std::env::vars_os())
    }

    /// The given variables, as the OS holds them ([`Env::process`]).
    pub fn from_os_pairs(
        pairs: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    ) -> Self {
        let mut env = Self::default();
        for (name, value) in pairs {
            let Ok(name) = name.into_string() else {
                continue;
            };
            match value.into_string() {
                Ok(value) => {
                    env.vars.insert(name, value);
                }
                Err(_) => {
                    env.not_utf8.insert(name);
                }
            }
        }
        env
    }

    /// The error for `name` having no value: missing, or set to a value that is not UTF-8.
    fn unset(&self, name: &str) -> EnvError {
        if self.not_utf8.contains(name) {
            EnvError::invalid(name, "(not UTF-8)", "UTF-8 text", "the value is not UTF-8")
        } else {
            EnvError::Missing { var: name.to_owned() }
        }
    }

    /// The given variables (the tests').
    pub fn from_pairs<K: Into<String>, V: Into<String>>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Self {
            vars: pairs.into_iter().map(|(name, value)| (name.into(), value.into())).collect(),
            not_utf8: BTreeSet::new(),
            read: RefCell::default(),
        }
    }

    /// The names of the variables read so far, whether or not they were set.
    pub fn read(&self) -> BTreeSet<String> {
        self.read.borrow().clone()
    }

    /// `name`'s value exactly as set (untrimmed, possibly blank); `None` when unset.
    pub fn raw(&self, name: &str) -> Option<&str> {
        self.read.borrow_mut().insert(name.to_owned());
        self.vars.get(name).map(String::as_str)
    }

    /// `name`'s value, trimmed; `None` when unset or blank.
    pub fn string(&self, name: &str) -> Option<String> {
        self.raw(name).map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned)
    }

    /// `name`'s value, trimmed; an error when unset or blank.
    pub fn required(&self, name: &str) -> Result<String, EnvError> {
        self.string(name).ok_or_else(|| self.unset(name))
    }

    /// `name`'s value exactly as set, blank included; an error only when unset. For the variables
    /// that have always accepted a blank value (see the caller's note).
    pub fn present(&self, name: &str) -> Result<String, EnvError> {
        self.raw(name).map(str::to_owned).ok_or_else(|| self.unset(name))
    }

    /// `name`'s comma-separated list (empty when unset).
    pub fn list(&self, name: &str) -> Vec<String> {
        parse_list(self.raw(name).unwrap_or_default())
    }

    /// `name`'s boolean (`true` or `false`); `None` when unset or blank.
    pub fn bool(&self, name: &str) -> Result<Option<bool>, EnvError> {
        parse_bool(name, self.raw(name))
    }

    /// `name`'s value parsed as a `T` (`expected` names the shape in an error); `None` when unset
    /// or blank.
    pub fn parse<T>(&self, name: &str, expected: &'static str) -> Result<Option<T>, EnvError>
    where
        T: FromStr,
        T::Err: Into<ParseFailure>,
    {
        parse_value(name, self.raw(name), expected)
    }

    /// `name`'s non-negative integer; `None` when unset or blank.
    pub fn u64(&self, name: &str) -> Result<Option<u64>, EnvError> {
        self.parse(name, NON_NEGATIVE_INTEGER)
    }

    /// `name`'s non-negative integer that fits in 32 bits; `None` when unset or blank.
    pub fn u32(&self, name: &str) -> Result<Option<u32>, EnvError> {
        self.parse(name, NON_NEGATIVE_INTEGER)
    }

    /// `name`'s JSON value (`expected` names its shape in an error); `None` when unset or blank.
    pub fn json<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
        expected: &'static str,
    ) -> Result<Option<T>, EnvError> {
        parse_json(name, self.raw(name), expected)
    }
}

/// Where a binary's Postgres connection URL comes from, read from its environment: a local
/// `DATABASE_URL`, else the RDS-managed secret CDK injects as `BIFFIX_DB_SECRET_ARN` (so nothing
/// sensitive lands in CloudFormation). `biffix_db::url::resolve` builds the URL from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DbConfig {
    /// The URL itself (`DATABASE_URL`).
    Url(String),
    /// The ARN of the RDS-managed secret to build it from (`BIFFIX_DB_SECRET_ARN`).
    Secret(String),
}

impl Default for DbConfig {
    fn default() -> Self {
        Self::Url(String::new())
    }
}

impl DbConfig {
    /// The variables it is read from.
    pub(crate) const VARS: [&str; 2] = ["DATABASE_URL", "BIFFIX_DB_SECRET_ARN"];

    /// From `env`: `DATABASE_URL` wins; a blank one counts as unset rather than yielding an empty
    /// URL that only fails later, when the pool connects.
    pub fn from_env(env: &Env) -> Result<Self, EnvError> {
        let url = env.string("DATABASE_URL");
        let secret = env.string("BIFFIX_DB_SECRET_ARN");
        match (url, secret) {
            (Some(url), _) => Ok(Self::Url(url)),
            (None, Some(arn)) => Ok(Self::Secret(arn)),
            // Set, but not to UTF-8: say so rather than that neither is set.
            (None, None) if env.not_utf8.contains("DATABASE_URL") => Err(env.unset("DATABASE_URL")),
            (None, None) if env.not_utf8.contains("BIFFIX_DB_SECRET_ARN") => {
                Err(env.unset("BIFFIX_DB_SECRET_ARN"))
            }
            (None, None) => Err(EnvError::MissingOneOf {
                vars: Self::VARS.map(String::from).to_vec(),
                why: "the database connection".into(),
            }),
        }
    }
}

/// The shape [`Env::u64`] and [`Env::u32`] name in an error.
const NON_NEGATIVE_INTEGER: &str = "a non-negative integer";

/// A comma-separated list (`a, b,,c` → `[a, b, c]`).
pub fn parse_list(raw: &str) -> Vec<String> {
    raw.split(',').map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned).collect()
}

/// `name`'s boolean from its value `raw`: `true` or `false`, trimmed; `None` when absent or blank.
pub fn parse_bool(name: &str, raw: Option<&str>) -> Result<Option<bool>, EnvError> {
    parse_value(name, raw, "true or false")
}

/// `name`'s value `raw` parsed as a `T`, trimmed; `None` when absent or blank.
pub(crate) fn parse_value<T>(
    name: &str,
    raw: Option<&str>,
    expected: &'static str,
) -> Result<Option<T>, EnvError>
where
    T: FromStr,
    T::Err: Into<ParseFailure>,
{
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => value
            .parse::<T>()
            .map(Some)
            .map_err(|error| EnvError::invalid(name, value, expected, error)),
    }
}

/// `name`'s JSON value `raw` (`expected` names its shape in an error), trimmed; `None` when absent
/// or blank.
pub fn parse_json<T: serde::de::DeserializeOwned>(
    name: &str,
    raw: Option<&str>,
    expected: &'static str,
) -> Result<Option<T>, EnvError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    serde_json::from_str(raw)
        .map(Some)
        .map_err(|error| EnvError::invalid(name, raw, expected, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Env {
        Env::from_pairs(pairs.iter().copied())
    }

    #[cfg(unix)]
    #[test]
    fn a_value_that_is_not_utf8_is_invalid_not_missing() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let bad = || OsString::from_vec(vec![0x66, 0xff]);
        let e = Env::from_os_pairs([
            (OsString::from("BAD"), bad()),
            (OsString::from("DATABASE_URL"), bad()),
            (OsString::from("OK"), OsString::from("v")),
        ]);
        assert_eq!(e.string("BAD"), None, "unset to a plain reader");
        assert_eq!(e.required("OK").unwrap(), "v");
        for error in [e.required("BAD"), e.present("BAD")] {
            let error = error.unwrap_err();
            assert!(matches!(error, EnvError::Invalid { .. }), "{error}");
            assert!(error.to_string().contains("not UTF-8"), "{error}");
        }
        assert!(matches!(e.required("UNSET"), Err(EnvError::Missing { .. })));
        assert!(matches!(
            DbConfig::from_env(&e),
            Err(EnvError::Invalid { var, .. }) if var == "DATABASE_URL"
        ));
    }

    #[test]
    fn a_list_is_trimmed_and_drops_empty_entries() {
        assert_eq!(parse_list("a, b,,c ,"), ["a", "b", "c"]);
        assert!(parse_list("").is_empty());
        assert!(parse_list(" , ").is_empty());
        assert_eq!(env(&[("L", " x ,y")]).list("L"), ["x", "y"]);
        assert!(env(&[]).list("L").is_empty());
    }

    #[test]
    fn a_number_is_absent_when_blank_and_an_error_names_the_expected_shape() {
        let e = env(&[("BLANK", " "), ("N", " 30 "), ("BAD", "thirty"), ("NEG", "-1")]);
        assert_eq!(e.u64("UNSET").unwrap(), None);
        assert_eq!(e.u64("BLANK").unwrap(), None);
        assert_eq!(e.u64("N").unwrap(), Some(30));
        assert_eq!(e.u32("N").unwrap(), Some(30));
        let error = e.u64("BAD").unwrap_err().to_string();
        assert!(error.contains("BAD"), "{error}");
        assert!(error.contains("non-negative integer"), "{error}");
        assert!(e.u64("NEG").is_err());
        let big = env(&[("BIG", "4294967296")]);
        assert_eq!(big.u64("BIG").unwrap(), Some(1 << 32));
        assert!(big.u32("BIG").is_err());
    }

    #[test]
    fn a_boolean_is_true_or_false() {
        let e = env(&[("T", " true"), ("F", "false"), ("B", ""), ("X", "1"), ("U", "TRUE")]);
        assert_eq!(e.bool("T").unwrap(), Some(true));
        assert_eq!(e.bool("F").unwrap(), Some(false));
        assert_eq!(e.bool("B").unwrap(), None);
        assert_eq!(e.bool("UNSET").unwrap(), None);
        let error = e.bool("X").unwrap_err().to_string();
        assert!(error.contains("X") && error.contains("true or false"), "{error}");
        assert!(e.bool("U").is_err());
    }

    #[test]
    fn strings_are_trimmed_and_blank_is_unset_except_for_present() {
        let e = env(&[("S", " v "), ("B", "  ")]);
        assert_eq!(e.string("S").as_deref(), Some("v"));
        assert_eq!(e.string("B"), None);
        assert_eq!(e.required("S").unwrap(), "v");
        assert!(matches!(e.required("B"), Err(EnvError::Missing { .. })));
        let error = e.required("UNSET").unwrap_err().to_string();
        assert!(error.contains("missing required env var UNSET"), "{error}");
        assert_eq!(e.present("B").unwrap(), "  ");
        assert!(e.present("UNSET").is_err());
        assert_eq!(e.raw("S"), Some(" v "));
    }

    #[test]
    fn json_is_parsed_into_its_type() {
        let e = env(&[("J", r#"{"a":1}"#), ("BAD", "{")]);
        let map: BTreeMap<String, u32> = e.json("J", "an object").unwrap().unwrap();
        assert_eq!(map["a"], 1);
        assert!(e.json::<BTreeMap<String, u32>>("UNSET", "x").unwrap().is_none());
        let error = e.json::<BTreeMap<String, u32>>("BAD", "an object").unwrap_err();
        assert_eq!(error.var(), "BAD");
        assert!(error.to_string().contains("an object"), "{error}");
    }

    #[test]
    fn every_read_is_recorded_and_errors_are_collected() {
        let e = env(&[("A", "x")]);
        let mut errors: Errors<EnvError> = Errors::default();
        let a = errors.take(e.required("A"));
        let b = errors.take(e.required("B"));
        let n = errors.take(e.u64("C"));
        assert_eq!((a.as_str(), b.as_str(), n), ("x", "", None));
        assert_eq!(e.read(), ["A", "B", "C"].map(String::from).into());
        let all = errors.finish(()).unwrap_err();
        assert_eq!(all.0.len(), 1);
        assert!(all.to_string().contains("B"), "{all}");
        assert!(Errors::<EnvError>::default().finish(1).is_ok());
    }

    #[test]
    fn database_url_wins_and_a_blank_one_is_unset() {
        let env =
            |pairs: &[(&str, &str)]| DbConfig::from_env(&Env::from_pairs(pairs.iter().copied()));
        assert_eq!(
            env(&[("DATABASE_URL", "postgres://x"), ("BIFFIX_DB_SECRET_ARN", "arn")]).unwrap(),
            DbConfig::Url("postgres://x".into())
        );
        assert_eq!(
            env(&[("DATABASE_URL", " "), ("BIFFIX_DB_SECRET_ARN", "arn")]).unwrap(),
            DbConfig::Secret("arn".into())
        );
        let error = env(&[]).unwrap_err().to_string();
        assert!(error.contains("DATABASE_URL or BIFFIX_DB_SECRET_ARN"), "{error}");
    }
}
