//! Credential storage: a `CredentialStore` trait with a mode-`0600` file backend, keyed per
//! profile.
//!
//! The file backend fails **closed** if it cannot apply owner-only permissions — a credential is
//! never written world-readable. Each credential also records the API base it was issued for, and
//! the token choke point refuses to send it anywhere else, so a token minted for one tenant is
//! never sent to another even when `--url` / `BIFFIX_API_URL` point elsewhere.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::CliError;
use crate::fsutil::{write_atomic, FileLock};
use crate::secret::Secret;

/// The refresh safety margin: refresh when within this window of expiry so an in-flight call does
/// not race the clock.
pub const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// Cached OAuth credentials for one profile. Tokens are [`Secret`] so they cannot leak through
/// `Debug`/`Display`; serde is transparent because the file is the secure store (see [`Secret`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: Secret<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<Secret<String>>,
    /// Access-token expiry, unix seconds.
    pub expires_at_unix: u64,
    /// Cognito `sub` — stable identity.
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// The API base URL these credentials were issued for (normalized, no trailing `/`).
    pub api_base_url: String,
}

impl Credentials {
    /// Who these credentials are for, as the CLI reports it: the email, else the subject.
    pub fn identity(&self) -> &str {
        self.email.as_deref().unwrap_or(&self.subject)
    }

    /// Whether the access token should be refreshed now: `now + margin >= expires_at`.
    /// Also true once already expired.
    pub fn needs_refresh(&self, now: SystemTime, margin: Duration) -> bool {
        let now_unix = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        now_unix.saturating_add(margin.as_secs()) >= self.expires_at_unix
    }

    /// Whether `other` is this same credential: issued for the same deployment, with the same
    /// tokens (a login or a refresh always issues a new access token).
    pub(crate) fn is_same(&self, other: &Credentials) -> bool {
        normalize_base(&self.api_base_url) == normalize_base(&other.api_base_url)
            && self.access_token == other.access_token
            && self.refresh_token == other.refresh_token
    }
}

/// The one normalization used to compare API bases: surrounding whitespace and trailing slashes
/// never make two spellings of one deployment differ.
pub(crate) fn normalize_base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialStoreError {
    #[error("cannot read credential store: {0}")]
    Read(String),
    #[error("cannot write credential store: {0}")]
    Write(String),
    #[error("credential store is corrupt: {0}")]
    Corrupt(String),
    #[error("refusing to store credentials without owner-only (0600) permissions: {0}")]
    Permissions(String),
}

impl From<CredentialStoreError> for CliError {
    fn from(e: CredentialStoreError) -> Self {
        CliError::Unclassified(e.to_string())
    }
}

/// Read/write cached credentials, keyed by profile name. `Send + Sync` so an API client can hold
/// one for the whole command and refresh through it.
pub trait CredentialStore: Send + Sync {
    fn load(&self, profile: &str) -> Result<Option<Credentials>, CredentialStoreError>;
    fn save(&self, profile: &str, creds: &Credentials) -> Result<(), CredentialStoreError>;
    /// Save `new` for `profile` only while the stored credential is still `current` (the same
    /// deployment and tokens), atomically with that check: `false`, and nothing written, when a
    /// logout, a newer login or another process's refresh replaced it meanwhile.
    fn replace(
        &self,
        profile: &str,
        current: &Credentials,
        new: &Credentials,
    ) -> Result<bool, CredentialStoreError>;
    fn clear(&self, profile: &str) -> Result<(), CredentialStoreError>;
}

#[derive(Serialize, Deserialize, Default)]
struct CredentialFile {
    #[serde(default)]
    profiles: BTreeMap<String, Credentials>,
}

/// The default backend: a single JSON file at `~/.config/biffix/credentials.json`, mode `0600`.
/// Every read-modify-write holds an advisory lock on a sibling lock file, so concurrent `biffix`
/// processes (a background refresh of one profile, a login of another) never drop each other's
/// update.
#[derive(Debug, Clone)]
pub struct FileCredentialStore {
    path: PathBuf,
}

impl FileCredentialStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The default path next to the config file.
    pub(crate) fn default_path() -> Result<PathBuf, CredentialStoreError> {
        crate::config::config_dir()
            .map(|d| d.join("credentials.json"))
            .map_err(|e| CredentialStoreError::Read(e.to_string()))
    }

    fn read_file(&self) -> Result<CredentialFile, CredentialStoreError> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| CredentialStoreError::Corrupt(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CredentialFile::default()),
            Err(e) => Err(CredentialStoreError::Read(e.to_string())),
        }
    }

    fn write_file(&self, file: &CredentialFile) -> Result<(), CredentialStoreError> {
        let text = serde_json::to_string_pretty(file)
            .map_err(|e| CredentialStoreError::Write(e.to_string()))?;
        // Only the owner-only check failing is the refusal; any other failure (a directory the
        // user cannot write, say) is reported as the I/O failure it is, naming the file.
        write_atomic(&self.path, text.as_bytes(), true).map_err(|e| {
            if crate::fsutil::is_not_owner_only(&e) {
                CredentialStoreError::Permissions(e.to_string())
            } else {
                CredentialStoreError::Write(format!("{}: {e}", self.path.display()))
            }
        })
    }

    fn lock(&self) -> Result<FileLock, CredentialStoreError> {
        FileLock::acquire(&self.path)
            .map_err(|e| CredentialStoreError::Write(format!("cannot lock: {e}")))
    }

    fn remove_file(&self) -> Result<(), CredentialStoreError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(CredentialStoreError::Write(e.to_string())),
        }
    }
}

impl CredentialStore for FileCredentialStore {
    fn load(&self, profile: &str) -> Result<Option<Credentials>, CredentialStoreError> {
        Ok(self.read_file()?.profiles.remove(profile))
    }

    fn save(&self, profile: &str, creds: &Credentials) -> Result<(), CredentialStoreError> {
        let _lock = self.lock()?;
        let mut file = self.read_file()?;
        file.profiles.insert(profile.to_string(), creds.clone());
        self.write_file(&file)
    }

    fn replace(
        &self,
        profile: &str,
        current: &Credentials,
        new: &Credentials,
    ) -> Result<bool, CredentialStoreError> {
        let _lock = self.lock()?;
        let mut file = self.read_file()?;
        if !file.profiles.get(profile).is_some_and(|c| c.is_same(current)) {
            return Ok(false);
        }
        file.profiles.insert(profile.to_string(), new.clone());
        self.write_file(&file)?;
        Ok(true)
    }

    /// Remove one profile's credentials. A corrupt file cannot be edited, and its other profiles
    /// are unreadable anyway, so it is removed outright rather than leaving `logout` unable to
    /// clear anything.
    fn clear(&self, profile: &str) -> Result<(), CredentialStoreError> {
        let _lock = self.lock()?;
        let mut file = match self.read_file() {
            Ok(file) => file,
            Err(CredentialStoreError::Corrupt(_)) => return self.remove_file(),
            Err(e) => return Err(e),
        };
        file.profiles.remove(profile);
        if file.profiles.is_empty() {
            self.remove_file()
        } else {
            self.write_file(&file)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(exp: u64) -> Credentials {
        Credentials {
            access_token: Secret::new(format!("access-{exp}")),
            refresh_token: Some(Secret::new(format!("refresh-{exp}"))),
            expires_at_unix: exp,
            subject: "user-1".into(),
            email: Some("a@b.c".into()),
            api_base_url: "https://tenant.example/api".into(),
        }
    }

    #[test]
    fn round_trips_every_field_per_profile_and_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("credentials.json"));
        assert!(store.load("default").unwrap().is_none());

        store.save("default", &creds(1000)).unwrap();
        store.save("staging", &creds(2000)).unwrap();
        // Every field survives, including the secrets themselves (a store that silently redacted
        // or dropped a token would otherwise pass on `expires_at_unix` alone).
        let got = store.load("default").unwrap().unwrap();
        assert_eq!(got.access_token.expose(), "access-1000");
        assert_eq!(got.refresh_token.as_ref().map(|t| t.expose().as_str()), Some("refresh-1000"));
        assert_eq!(got.expires_at_unix, 1000);
        assert_eq!(got.subject, "user-1");
        assert_eq!(got.email.as_deref(), Some("a@b.c"));
        assert_eq!(got.api_base_url, "https://tenant.example/api");
        assert_eq!(store.load("staging").unwrap().unwrap().access_token.expose(), "access-2000");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("credentials.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        store.clear("default").unwrap();
        assert!(store.load("default").unwrap().is_none());
        assert!(store.load("staging").unwrap().is_some());
    }

    #[test]
    fn a_save_is_atomic_and_leaves_no_temp_litter() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("credentials.json"));
        store.save("default", &creds(1000)).unwrap();
        // Overwriting an existing store still leaves only the file (and its lock) behind.
        store.save("staging", &creds(2000)).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![".credentials.json.lock".to_string(), "credentials.json".to_string()],
            "stray files: {names:?}"
        );
    }

    #[test]
    fn concurrent_saves_of_different_profiles_all_survive() {
        // Without the lock around read-modify-write, the last rename wins and drops the others.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let handles: Vec<_> = (0..16u64)
            .map(|i| {
                let store = FileCredentialStore::new(path.clone());
                std::thread::spawn(move || store.save(&format!("p{i}"), &creds(i)).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let store = FileCredentialStore::new(path);
        for i in 0..16u64 {
            assert!(store.load(&format!("p{i}")).unwrap().is_some(), "profile p{i} was lost");
        }
    }

    #[test]
    fn a_replace_writes_only_over_the_credential_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCredentialStore::new(dir.path().join("credentials.json"));
        let (old, new, newer) = (creds(1000), creds(2000), creds(3000));
        // Nothing stored (a logout): nothing written.
        assert!(!store.replace("default", &old, &new).unwrap());
        assert!(store.load("default").unwrap().is_none());
        store.save("default", &old).unwrap();
        assert!(store.replace("default", &old, &new).unwrap());
        assert_eq!(store.load("default").unwrap().unwrap().access_token.expose(), "access-2000");
        // `old` is no longer what is stored: the newer credential stays.
        assert!(!store.replace("default", &old, &newer).unwrap());
        assert_eq!(store.load("default").unwrap().unwrap().access_token.expose(), "access-2000");
    }

    /// A directory the user cannot write is an I/O failure naming the file, not the owner-only
    /// refusal (which would send them looking at the file's mode).
    #[cfg(unix)]
    #[test]
    fn an_unwritable_directory_is_a_write_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = FileCredentialStore::new(path.clone());
        store.save("default", &creds(1000)).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = store.save("default", &creds(2000));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(&result, Err(CredentialStoreError::Write(m))
                if m.contains(&path.display().to_string())),
            "{result:?}"
        );
    }

    #[test]
    fn clearing_a_corrupt_store_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, "garbage").unwrap();
        let store = FileCredentialStore::new(path.clone());
        assert!(matches!(store.load("default"), Err(CredentialStoreError::Corrupt(_))));
        store.clear("default").unwrap();
        assert!(!path.exists());
        assert!(store.load("default").unwrap().is_none());
    }

    #[test]
    fn a_token_is_refreshed_from_the_margin_before_it_expires() {
        let c = creds(1000);
        let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
        // Well before expiry minus margin: no refresh.
        assert!(!c.needs_refresh(at(900), REFRESH_MARGIN));
        // Exactly at the boundary (now + margin == expires_at): refresh.
        assert!(c.needs_refresh(at(940), REFRESH_MARGIN));
        // Past expiry: refresh.
        assert!(c.needs_refresh(at(2000), REFRESH_MARGIN));
    }

    #[test]
    fn base_normalization_ignores_trailing_slashes() {
        assert_eq!(normalize_base("https://x.example/api/"), "https://x.example/api");
        assert_eq!(normalize_base(" https://x.example/api "), "https://x.example/api");
    }
}
