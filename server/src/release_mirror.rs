//! Verified, anonymous mirror of TokenLens public GitHub releases.
//!
//! Nothing from the updater manifest is used as a download location unless it
//! names the expected asset in the expected versioned GitHub Release. A new
//! release is complete on disk before the `current` pointer is replaced.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use sha2::{Digest, Sha256};

pub const UPSTREAM_MANIFEST: &str =
    "https://github.com/Dawn-4ever1/TokenLens/releases/latest/download/latest.json";
pub const DEFAULT_PUBLIC_BASE: &str = "https://tokenlens.online";
/// Public key, identical to `plugins.updater.pubkey` in tauri.conf.json.
pub const UPDATER_PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEQ0MEVCRjZFMzI5QTBENTQKUldSVURab3licjhPMU9FYVV1clZsdWgzWXF5cWREZzVPQmMreGNBQ2E2VWVBRVFmaG81czM1Tk0K";
pub const MANIFEST_FILE: &str = "latest.json";
pub const SUMS_FILE: &str = "SHA256SUMS.txt";
pub const MAX_ASSET_BYTES: usize = 15 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_SUMS_BYTES: usize = 16 * 1024;
pub const MAX_SIGNATURE_BYTES: usize = 16 * 1024;
pub const SYNC_INTERVAL: Duration = Duration::from_secs(60 * 60);
pub const FIRST_RETRY: Duration = Duration::from_secs(60);
pub const MAX_RETRY: Duration = Duration::from_secs(15 * 60);

const RELEASE_PATH: &str = "/Dawn-4ever1/TokenLens/releases/download/";
const REDIRECT_HOST: &str = "release-assets.githubusercontent.com";
const MAX_REDIRECTS: usize = 5;
const WINDOWS: &str = "windows-x86_64";
const MACOS: &str = "darwin-aarch64";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Io,
    Network,
    Response,
    Manifest,
    Url,
    Checksum,
    Signature,
    SignedVersion,
    Stale,
}

impl ErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Network => "network",
            Self::Response => "http_response",
            Self::Manifest => "manifest",
            Self::Url => "url",
            Self::Checksum => "checksum",
            Self::Signature => "signature",
            Self::SignedVersion => "signed_version",
            Self::Stale => "stale_release",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("release mirror operation failed")]
pub struct MirrorError {
    pub kind: ErrorKind,
}

impl MirrorError {
    fn new(kind: ErrorKind) -> Self {
        Self { kind }
    }
}

impl From<io::Error> for MirrorError {
    fn from(_: io::Error) -> Self {
        Self::new(ErrorKind::Io)
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdaterManifest {
    version: String,
    notes: String,
    pub_date: String,
    platforms: BTreeMap<String, UpdaterPlatform>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdaterPlatform {
    signature: String,
    url: String,
}

struct Candidate {
    version: Version,
    manifest: Vec<u8>,
    files: BTreeMap<String, Vec<u8>>,
}

pub struct Snapshot {
    version: Version,
    manifest: Vec<u8>,
    directory: PathBuf,
    locations: BTreeMap<String, String>,
    // SHA256SUMS authenticates the other six files. This digest authenticates
    // SHA256SUMS itself after it has validated every one of those entries.
    sums_digest: String,
    checksums: BTreeMap<String, String>,
}

impl Snapshot {
    pub fn version(&self) -> String {
        self.version.to_string()
    }

    pub fn manifest(&self) -> &[u8] {
        &self.manifest
    }

    pub fn location(&self, filename: &str) -> Option<&str> {
        self.locations.get(filename).map(String::as_str)
    }

    pub fn file_path(&self, filename: &str) -> Option<PathBuf> {
        asset_names(&self.version)
            .contains(&filename.to_owned())
            .then(|| self.directory.join(filename))
    }

    #[doc(hidden)]
    pub fn checksum_matches(&self, filename: &str, bytes: &[u8]) -> bool {
        if filename == SUMS_FILE {
            return constant_time_equal(self.sums_digest.as_bytes(), hex_sha256(bytes).as_bytes());
        }
        self.checksums.get(filename).is_some_and(|expected| {
            constant_time_equal(expected.as_bytes(), hex_sha256(bytes).as_bytes())
        })
    }
}

/// The currently published immutable release. Cloning this handle is cheap;
/// no maintenance lock is taken while an HTTP request reads an asset.
pub struct ReleaseMirror {
    root: PathBuf,
    public_base: String,
    public_key: String,
    source: Arc<dyn ReleaseSource>,
    _storage_lock: fs::File,
    current: RwLock<Option<Arc<Snapshot>>>,
    maintenance: tokio::sync::Mutex<()>,
}

/// Injection point for deterministic offline tests. Production always uses
/// `GitHubSource`; request parameters can never select an upstream server.
#[doc(hidden)]
pub trait ReleaseSource: Send + Sync + 'static {
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, MirrorError>;
}

struct GitHubSource {
    agent: ureq::Agent,
}

impl GitHubSource {
    fn new() -> Self {
        Self {
            agent: ureq::Agent::new_with_config(
                ureq::Agent::config_builder()
                    .https_only(true)
                    .max_redirects(0)
                    .http_status_as_error(false)
                    .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
                    .proxy(None)
                    .timeout_connect(Some(Duration::from_secs(5)))
                    .timeout_global(Some(Duration::from_secs(45)))
                    .build(),
            ),
        }
    }
}

impl ReleaseSource for GitHubSource {
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, MirrorError> {
        let mut url = url.to_owned();
        for _ in 0..=MAX_REDIRECTS {
            // The request carries only these generic headers: no cookies,
            // authorization, device information or user data.
            let response = self
                .agent
                .get(&url)
                .header("Accept", "*/*")
                .header("Cache-Control", "no-cache")
                .call()
                .map_err(|_| MirrorError::new(ErrorKind::Network))?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| MirrorError::new(ErrorKind::Url))?;
                url = checked_redirect(&url, location)?;
                continue;
            }
            if response.status() != ureq::http::StatusCode::OK {
                return Err(MirrorError::new(ErrorKind::Response));
            }
            return response
                .into_body()
                .with_config()
                .limit(limit as u64)
                .read_to_vec()
                .map_err(|_| MirrorError::new(ErrorKind::Response));
        }
        Err(MirrorError::new(ErrorKind::Url))
    }
}

/// `base` is operator configuration, never the incoming Host header.
pub fn valid_public_base(base: &str) -> bool {
    let Ok(uri) = base.parse::<ureq::http::Uri>() else {
        return false;
    };
    uri.scheme_str() == Some("https")
        && uri.host().is_some()
        && uri.authority().is_some_and(|authority| {
            authority.port().is_none() && authority.as_str() == authority.host()
        })
        && uri.path() == "/"
        && uri.query().is_none()
        && uri.host().is_none_or(|host| {
            host != "localhost"
                && !host.ends_with(".localhost")
                && !host.ends_with('.')
                && host.parse::<std::net::IpAddr>().is_err()
        })
}

fn release_path(version: &Version, filename: &str) -> String {
    format!("{RELEASE_PATH}v{version}/{filename}")
}

fn release_url(version: &Version, filename: &str) -> String {
    format!("https://github.com{}", release_path(version, filename))
}

fn asset_names(version: &Version) -> [String; 7] {
    let windows = format!("TokenLens_{version}_x64-setup.exe");
    let mac = format!("TokenLens_{version}_aarch64.app.tar.gz");
    [
        MANIFEST_FILE.to_owned(),
        SUMS_FILE.to_owned(),
        windows.clone(),
        format!("{windows}.sig"),
        mac.clone(),
        format!("{mac}.sig"),
        format!("TokenLens_{version}_aarch64.dmg"),
    ]
}

fn uri_allowed(uri: &ureq::http::Uri) -> bool {
    if uri.scheme_str() != Some("https") || uri.authority().is_none() {
        return false;
    }
    let authority = uri.authority().expect("checked above");
    if authority.port().is_some() || authority.as_str() != authority.host() {
        // Rejects userinfo, non-default ports, trailing-dot aliases and any
        // form of IP/localhost endpoint, including in redirects.
        return false;
    }
    match authority.host() {
        // Release pages carry no query; their assets do carry GitHub's signed
        // download token in the query, which is required and short-lived.
        "github.com" => uri.query().is_none() && uri.path().starts_with(RELEASE_PATH),
        REDIRECT_HOST => {
            uri.query().is_some() && uri.path().starts_with("/github-production-release-asset/")
        }
        _ => false,
    }
}

/// Only the fixed Release owner/repository path or GitHub's asset host can be
/// a redirect target. Relative redirects are resolved without URL crates.
pub fn checked_redirect(current: &str, location: &str) -> Result<String, MirrorError> {
    if location.is_empty()
        || location.len() > 4096
        || location.chars().any(|ch| ch.is_control() || ch == '\\')
        || location.contains('#')
    {
        return Err(MirrorError::new(ErrorKind::Url));
    }
    let base = current
        .parse::<ureq::http::Uri>()
        .map_err(|_| MirrorError::new(ErrorKind::Url))?;
    let uri = if let Ok(uri) = location.parse::<ureq::http::Uri>() {
        if uri.scheme().is_some() || uri.authority().is_some() {
            uri
        } else {
            let authority = base
                .authority()
                .ok_or_else(|| MirrorError::new(ErrorKind::Url))?;
            let path = if location.starts_with('/') {
                location.to_owned()
            } else {
                let directory = base.path().rsplit_once('/').map_or("", |(dir, _)| dir);
                format!("{directory}/{location}")
            };
            format!("https://{authority}{path}")
                .parse()
                .map_err(|_| MirrorError::new(ErrorKind::Url))?
        }
    } else {
        return Err(MirrorError::new(ErrorKind::Url));
    };
    if !uri_allowed(&uri) {
        return Err(MirrorError::new(ErrorKind::Url));
    }
    Ok(uri.to_string())
}

impl ReleaseMirror {
    /// Opens durable storage. Unset `RELEASE_MIRROR_DIR` (or `off`) is the
    /// only opt-out in `Config`; callers that omit the mirror make no network
    /// requests. An existing complete release remains available immediately.
    pub fn open(root: PathBuf, public_base: &str) -> Result<Arc<Self>, MirrorError> {
        Self::open_with(
            root,
            public_base,
            UPDATER_PUBLIC_KEY.to_owned(),
            Arc::new(GitHubSource::new()),
        )
    }

    #[doc(hidden)]
    pub fn open_for_test(
        root: PathBuf,
        public_base: &str,
        public_key: &str,
        source: Arc<dyn ReleaseSource>,
    ) -> Result<Arc<Self>, MirrorError> {
        Self::open_with(root, public_base, public_key.to_owned(), source)
    }

    fn open_with(
        root: PathBuf,
        public_base: &str,
        public_key: String,
        source: Arc<dyn ReleaseSource>,
    ) -> Result<Arc<Self>, MirrorError> {
        if !valid_public_base(public_base) {
            return Err(MirrorError::new(ErrorKind::Url));
        }
        fs::create_dir_all(root.join("versions"))?;
        fs::create_dir_all(root.join("staging"))?;
        let storage_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join(".mirror.lock"))?;
        // The embedded local server opens this library several times during
        // its lifetime; only explicitly configured mirror instances take this
        // lock, and a second live owner cannot race staging/publication.
        fs2::FileExt::lock_exclusive(&storage_lock).map_err(|_| MirrorError::new(ErrorKind::Io))?;
        let mirror = Arc::new(Self {
            root,
            public_base: public_base.trim_end_matches('/').to_owned(),
            public_key,
            source,
            _storage_lock: storage_lock,
            current: RwLock::new(None),
            maintenance: tokio::sync::Mutex::new(()),
        });
        mirror.load_published()?;
        Ok(mirror)
    }

    pub fn current(&self) -> Option<Arc<Snapshot>> {
        self.current.read().ok().and_then(|current| current.clone())
    }

    /// Fully downloads and verifies a candidate before changing `current`.
    /// Concurrent manual/scheduled calls share one maintenance lock.
    pub async fn sync(self: &Arc<Self>) -> Result<Version, MirrorError> {
        let _maintenance = self.maintenance.lock().await;
        let mirror = Arc::clone(self);
        tokio::task::spawn_blocking(move || mirror.sync_locked())
            .await
            .map_err(|_| MirrorError::new(ErrorKind::Io))?
    }

    fn sync_locked(self: &Arc<Self>) -> Result<Version, MirrorError> {
        let manifest = self.source.get(UPSTREAM_MANIFEST, MAX_MANIFEST_BYTES)?;
        let candidate = verify_manifest(manifest)?;
        if self
            .current()
            .is_some_and(|current| candidate.version <= current.version)
        {
            if self
                .current()
                .is_some_and(|current| candidate.version < current.version)
            {
                return Err(MirrorError::new(ErrorKind::Stale));
            }
            return Ok(candidate.version);
        }
        let candidate = self.download_candidate(candidate)?;
        let version = candidate.version.clone();
        self.publish(candidate)?;
        Ok(version)
    }

    fn download_candidate(&self, mut candidate: Candidate) -> Result<Candidate, MirrorError> {
        for filename in asset_names(&candidate.version) {
            if filename == MANIFEST_FILE {
                continue;
            }
            let limit = if filename == SUMS_FILE {
                MAX_SUMS_BYTES
            } else if filename.ends_with(".sig") {
                MAX_SIGNATURE_BYTES
            } else {
                MAX_ASSET_BYTES
            };
            let bytes = self
                .source
                .get(&release_url(&candidate.version, &filename), limit)?;
            if filename == SUMS_FILE {
                // The untrusted manifest has not authorized any artifact yet;
                // authenticate the original SHA256SUMS entry for it first.
                let expected = parse_sums(&bytes)?
                    .get(MANIFEST_FILE)
                    .cloned()
                    .ok_or_else(|| MirrorError::new(ErrorKind::Checksum))?;
                if !constant_time_equal(
                    expected.as_bytes(),
                    hex_sha256(&candidate.manifest).as_bytes(),
                ) {
                    return Err(MirrorError::new(ErrorKind::Checksum));
                }
            }
            if !filename.ends_with(".sig") && filename != SUMS_FILE {
                let expected = parse_sums(candidate.files[SUMS_FILE].as_slice())?
                    .remove(&filename)
                    .ok_or_else(|| MirrorError::new(ErrorKind::Checksum))?;
                let actual = hex_sha256(&bytes);
                if !constant_time_equal(expected.as_bytes(), actual.as_bytes()) {
                    return Err(MirrorError::new(ErrorKind::Checksum));
                }
            }
            if filename.ends_with(".sig") {
                let expected = candidate
                    .files
                    .get(&filename)
                    .ok_or_else(|| MirrorError::new(ErrorKind::Signature))?;
                let encoded = std::str::from_utf8(&bytes)
                    .map_err(|_| MirrorError::new(ErrorKind::Signature))?;
                let decoded = decode_signature(encoded)?;
                let manifest_signature = std::str::from_utf8(expected)
                    .map_err(|_| MirrorError::new(ErrorKind::Signature))?;
                let manifest_signature = decode_signature(manifest_signature)?;
                if !constant_time_equal(decoded.as_bytes(), manifest_signature.as_bytes()) {
                    return Err(MirrorError::new(ErrorKind::Signature));
                }
                candidate.files.insert(filename, bytes);
                continue;
            }
            candidate.files.insert(filename, bytes);
        }
        verify_updaters(&candidate, &self.public_key)?;
        // Both SHA256SUMS and the manifest itself are covered by the original,
        // unmodified upstream checksum file (verified above).
        Ok(candidate)
    }

    fn publish(self: &Arc<Self>, candidate: Candidate) -> Result<(), MirrorError> {
        let version = candidate.version.to_string();
        let final_dir = self.root.join("versions").join(&version);
        if final_dir.exists() {
            // A crashed earlier attempt must never be mistaken for a verified
            // published version merely because its directory survived.
            fs::remove_dir_all(&final_dir)?;
        }
        let staging = self.root.join("staging").join(format!("v{version}"));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir(&staging)?;
        let result = (|| {
            for (filename, bytes) in &candidate.files {
                write_new(&staging.join(filename), bytes)?;
            }
            let pointer = self.root.join("current.json");
            let published = Published {
                version: version.clone(),
                files: asset_names(&candidate.version).to_vec(),
            };
            let next_pointer = self.root.join("current.next");
            if next_pointer.exists() {
                fs::remove_file(&next_pointer)?;
            }
            write_json_new(&next_pointer, &published)?;
            sync_dir(&staging)?;
            sync_dir(staging.parent().expect("staging child"))?;
            fs::rename(&staging, &final_dir)?;
            sync_dir(final_dir.parent().expect("version child"))?;
            // One rename atomically changes which complete immutable directory
            // is live. Never remove the old current directory first.
            fs::rename(&next_pointer, &pointer)?;
            sync_dir(&self.root)?;
            let snapshot = Arc::new(Snapshot {
                manifest: public_manifest(&candidate, &self.public_base)?,
                version: candidate.version.clone(),
                directory: final_dir,
                locations: asset_locations(&candidate.version, &self.public_base),
                sums_digest: hex_sha256(&candidate.files[SUMS_FILE]),
                checksums: parse_sums(candidate.files[SUMS_FILE].as_slice())?,
            });
            *self
                .current
                .write()
                .map_err(|_| MirrorError::new(ErrorKind::Io))? = Some(snapshot);
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(staging);
        }
        result
    }

    fn load_published(&self) -> Result<(), MirrorError> {
        let pointer = self.root.join("current.json");
        let Ok(metadata) = fs::symlink_metadata(&pointer) else {
            return Ok(());
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(MirrorError::new(ErrorKind::Io));
        }
        let published: Published = serde_json::from_slice(&fs::read(&pointer)?)
            .map_err(|_| MirrorError::new(ErrorKind::Manifest))?;
        let version = Version::parse(&published.version)
            .map_err(|_| MirrorError::new(ErrorKind::Manifest))?;
        if version.build != semver::BuildMetadata::EMPTY {
            return Err(MirrorError::new(ErrorKind::Manifest));
        }
        let expected = asset_names(&version).to_vec();
        if published.files != expected {
            return Err(MirrorError::new(ErrorKind::Manifest));
        }
        let directory = self.root.join("versions").join(version.to_string());
        let directory_meta = fs::symlink_metadata(&directory)?;
        if !directory_meta.is_dir() || directory_meta.file_type().is_symlink() {
            return Err(MirrorError::new(ErrorKind::Io));
        }
        // Startup never serves a half-directory left by an interrupted deploy.
        // Files were authenticated before publication; require exact sizes and
        // hashes again before loading the pointer.
        let sums_bytes = fs::read(directory.join(SUMS_FILE))?;
        let sums = parse_sums(&sums_bytes)?;
        let mut updater_bytes = BTreeMap::new();
        updater_bytes.insert(SUMS_FILE.to_owned(), sums_bytes);
        for filename in expected {
            if filename == SUMS_FILE {
                continue;
            }
            let path = directory.join(&filename);
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(MirrorError::new(ErrorKind::Io));
            }
            let bytes = fs::read(path)?;
            let Some(expected) = sums.get(&filename) else {
                return Err(MirrorError::new(ErrorKind::Checksum));
            };
            if !constant_time_equal(expected.as_bytes(), hex_sha256(&bytes).as_bytes()) {
                return Err(MirrorError::new(ErrorKind::Checksum));
            }
            updater_bytes.insert(filename, bytes);
        }
        if sums.len() != 6 {
            return Err(MirrorError::new(ErrorKind::Checksum));
        }
        // It authenticates the six other assets; its own bytes are saved in
        // Snapshot after every signed artifact has passed verification.
        let sums_digest = hex_sha256(&updater_bytes[SUMS_FILE]);
        let candidate = Candidate {
            version,
            manifest: updater_bytes[MANIFEST_FILE].clone(),
            files: updater_bytes,
        };
        verify_updaters(&candidate, &self.public_key)?;
        let snapshot = Snapshot {
            manifest: public_manifest(&candidate, &self.public_base)?,
            version: candidate.version.clone(),
            directory,
            locations: asset_locations(&candidate.version, &self.public_base),
            sums_digest,
            checksums: sums,
        };
        *self
            .current
            .write()
            .map_err(|_| MirrorError::new(ErrorKind::Io))? = Some(Arc::new(snapshot));
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Published {
    version: String,
    files: Vec<String>,
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), MirrorError> {
    let mut file = fs::File::create_new(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_json_new<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), MirrorError> {
    let bytes = serde_json::to_vec(value).map_err(|_| MirrorError::new(ErrorKind::Io))?;
    write_new(path, &bytes)
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<(), MirrorError> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> Result<(), MirrorError> {
    Ok(())
}

fn public_manifest(candidate: &Candidate, public_base: &str) -> Result<Vec<u8>, MirrorError> {
    let mut value: serde_json::Value = serde_json::from_slice(&candidate.manifest)
        .map_err(|_| MirrorError::new(ErrorKind::Manifest))?;
    let locations = asset_locations(&candidate.version, public_base);
    for (platform, filename) in [
        (
            WINDOWS,
            format!("TokenLens_{}_x64-setup.exe", candidate.version),
        ),
        (
            MACOS,
            format!("TokenLens_{}_aarch64.app.tar.gz", candidate.version),
        ),
    ] {
        value["platforms"][platform]["url"] =
            serde_json::Value::String(locations[&filename].clone());
    }
    let mut bytes =
        serde_json::to_vec_pretty(&value).map_err(|_| MirrorError::new(ErrorKind::Manifest))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn asset_locations(version: &Version, public_base: &str) -> BTreeMap<String, String> {
    asset_names(version)
        .into_iter()
        .map(|filename| {
            let location = format!("{public_base}/downloads/v{version}/{filename}");
            (filename, location)
        })
        .collect()
}

fn verify_manifest(manifest: Vec<u8>) -> Result<Candidate, MirrorError> {
    let parsed: UpdaterManifest =
        serde_json::from_slice(&manifest).map_err(|_| MirrorError::new(ErrorKind::Manifest))?;
    let version =
        Version::parse(&parsed.version).map_err(|_| MirrorError::new(ErrorKind::Manifest))?;
    if parsed.platforms.len() != 2
        || parsed.notes.len() > 32 * 1024
        || parsed.pub_date.len() > 64
        || parsed.pub_date.is_empty()
    {
        return Err(MirrorError::new(ErrorKind::Manifest));
    }
    for (platform, expected_name) in [
        (WINDOWS, format!("TokenLens_{version}_x64-setup.exe")),
        (MACOS, format!("TokenLens_{version}_aarch64.app.tar.gz")),
    ] {
        let entry = parsed
            .platforms
            .get(platform)
            .ok_or_else(|| MirrorError::new(ErrorKind::Manifest))?;
        let uri = entry
            .url
            .parse::<ureq::http::Uri>()
            .map_err(|_| MirrorError::new(ErrorKind::Url))?;
        if !uri_allowed(&uri)
            || uri.host() != Some("github.com")
            || uri.path() != release_path(&version, &expected_name)
        {
            return Err(MirrorError::new(ErrorKind::Url));
        }
        let decoded = decode_signature(&entry.signature)?;
        if Signature::decode(&decoded).is_err() {
            return Err(MirrorError::new(ErrorKind::Signature));
        }
    }
    let windows_name = format!("TokenLens_{version}_x64-setup.exe");
    let mac_name = format!("TokenLens_{version}_aarch64.app.tar.gz");
    let mut files = BTreeMap::new();
    files.insert(MANIFEST_FILE.to_owned(), manifest.clone());
    for (platform, filename) in [(WINDOWS, windows_name), (MACOS, mac_name)] {
        let signature = parsed.platforms[platform].signature.clone();
        files.insert(format!("{filename}.sig"), signature.into_bytes());
    }
    Ok(Candidate {
        version,
        manifest,
        files,
    })
}

fn decode_signature(encoded: &str) -> Result<String, MirrorError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| MirrorError::new(ErrorKind::Signature))?;
    if bytes.len() > MAX_SIGNATURE_BYTES
        || base64::engine::general_purpose::STANDARD.encode(&bytes) != encoded
    {
        return Err(MirrorError::new(ErrorKind::Signature));
    }
    String::from_utf8(bytes).map_err(|_| MirrorError::new(ErrorKind::Signature))
}

fn verify_updaters(candidate: &Candidate, public_key: &str) -> Result<(), MirrorError> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(public_key)
        .map_err(|_| MirrorError::new(ErrorKind::Signature))?;
    let key_text =
        String::from_utf8(decoded).map_err(|_| MirrorError::new(ErrorKind::Signature))?;
    let key = PublicKey::decode(&key_text).map_err(|_| MirrorError::new(ErrorKind::Signature))?;
    for filename in [
        format!("TokenLens_{}_x64-setup.exe", candidate.version),
        format!("TokenLens_{}_aarch64.app.tar.gz", candidate.version),
    ] {
        let bytes = &candidate.files[&filename];
        let encoded = std::str::from_utf8(&candidate.files[&format!("{filename}.sig")])
            .map_err(|_| MirrorError::new(ErrorKind::Signature))?;
        let signature = decode_signature(encoded)?;
        let signature =
            Signature::decode(&signature).map_err(|_| MirrorError::new(ErrorKind::Signature))?;
        // Match the desktop updater: legacy signatures stay acceptable.
        key.verify(bytes, &signature, true)
            .map_err(|_| MirrorError::new(ErrorKind::Signature))?;
        let versions = signature
            .trusted_comment()
            .split('\t')
            .filter_map(|field| field.strip_prefix("version:"))
            .collect::<Vec<_>>();
        let expected = candidate.version.to_string();
        if versions.len() != 1
            || versions[0].trim_start_matches('v') != expected
            || (versions[0].starts_with('v') && versions[0].len() != expected.len() + 1)
        {
            return Err(MirrorError::new(ErrorKind::SignedVersion));
        }
    }
    Ok(())
}

fn parse_sums(bytes: &[u8]) -> Result<BTreeMap<String, String>, MirrorError> {
    let text = std::str::from_utf8(bytes).map_err(|_| MirrorError::new(ErrorKind::Checksum))?;
    let mut sums = BTreeMap::new();
    for line in text.lines() {
        let Some((digest, filename)) = line.split_once("  ") else {
            return Err(MirrorError::new(ErrorKind::Checksum));
        };
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || filename.is_empty()
            || filename.contains(['/', '\\', '\0'])
            || sums
                .insert(filename.to_owned(), digest.to_owned())
                .is_some()
        {
            return Err(MirrorError::new(ErrorKind::Checksum));
        }
    }
    if sums.len() != 6 {
        return Err(MirrorError::new(ErrorKind::Checksum));
    }
    Ok(sums)
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0, |difference, (a, b)| difference | (a ^ b))
            == 0
}

/// Startup immediately syncs; healthy refreshes happen hourly. Failures use
/// exponential backoff and never clear the last published snapshot.
pub async fn maintain(mirror: Arc<ReleaseMirror>) {
    let mut retry = FIRST_RETRY;
    loop {
        match mirror.sync().await {
            Ok(version) => {
                tracing::info!(version = %version, "verified release mirror synchronized");
                retry = FIRST_RETRY;
                tokio::time::sleep(SYNC_INTERVAL).await;
            }
            Err(error) => {
                tracing::warn!(
                    kind = error.kind.as_str(),
                    "release mirror sync failed; keeping previous release"
                );
                tokio::time::sleep(retry).await;
                retry = retry.saturating_mul(2).min(MAX_RETRY);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_base_and_redirects_are_strict() {
        assert!(valid_public_base(DEFAULT_PUBLIC_BASE));
        for base in [
            "http://tokenlens.online",
            "https://user@tokenlens.online",
            "https://tokenlens.online:443",
            "https://tokenlens.online/path",
            "https://localhost",
            "https://127.0.0.1",
        ] {
            assert!(!valid_public_base(base), "{base}");
        }
        assert!(
            checked_redirect(
                UPSTREAM_MANIFEST,
                "https://release-assets.githubusercontent.com/github-production-release-asset/asset?sp=r&sig=abc"
            )
            .is_ok()
        );
        assert!(
            checked_redirect(
                UPSTREAM_MANIFEST,
                "https://release-assets.githubusercontent.com/github-production-release-asset/asset"
            )
            .is_err()
        );
        for location in [
            "http://github.com/Dawn-4ever1/TokenLens/releases/download/v1.2.3/a",
            "https://localhost/a",
            "https://127.0.0.1/a",
            "https://github.com.evil.example/Dawn-4ever1/TokenLens/releases/download/v1/a",
            "https://github.com@evil.example/Dawn-4ever1/TokenLens/releases/download/v1/a",
            "https://release-assets.githubusercontent.com.evil.example/a",
            "https://release-assets.githubusercontent.com/a?query=secret",
            "https://github.com/other/repository/releases/download/v1/a",
            "../../../../etc/passwd",
        ] {
            assert!(
                checked_redirect(UPSTREAM_MANIFEST, location).is_err(),
                "{location}"
            );
        }
    }

    #[test]
    fn production_key_matches_tauri_configuration() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../app/src-tauri/tauri.conf.json"))
                .expect("tauri config");
        assert_eq!(UPDATER_PUBLIC_KEY, config["plugins"]["updater"]["pubkey"]);
    }
}
