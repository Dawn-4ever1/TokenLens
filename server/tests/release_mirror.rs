//! Complete release-mirror tests. All signatures were generated once from an
//! ephemeral Ed25519 test key; no network or production private key is used.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use common::Harness;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokenlens_server::release_mirror::{
    self, ErrorKind, MirrorError, ReleaseMirror, ReleaseSource, UPSTREAM_MANIFEST,
};

const PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IFRva2VuTGVucyBvZmZsaW5lIG1pcnJvciB0ZXN0IGtleQpSV1JNVGxOVVJWTlVNYU95cTRsTWJNekNyWC9VbzZSQmR1bkVEa2p6eFhLRHBLZ3diK05XNU1ySwo=";
const WINDOWS_SIG_123: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG9mZmxpbmUgdGVzdCBzaWduYXR1cmUKUlVSTVRsTlVSVk5VTVdJaGJ2MEZrWHNiNDZwcXIrOXIyclhOSGtUcG9Fb0wzU2dnQzJ0SnpHd3Z6bXRnN2h0QUJNQkptZ2VCSXRRQndsNjhwYmxXL1UrVzRhdHFTRkpVS0FFPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxCWZpbGU6b2ZmbGluZS1maXh0dXJlCXZlcnNpb246MS4yLjMKMWNnZThYYUtLL2VMZ3VIRG5mVldTTkZKandzdzZGcEhINXprNG5NSkE2N0o3eFhObW02VVU4N0ZvL1hHMld6ZFRsSEgxUVNpbzR2UDFlZzFXa0JRQUE9PQo=";
const MAC_SIG_123: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG9mZmxpbmUgdGVzdCBzaWduYXR1cmUKUlVSTVRsTlVSVk5VTVE0bkQ5OFQxK29qMDIzY1h6dE1wNGFjVXRLb0lvUGNpY2YyK1RhZ2lBVGxQcWZPVW5hTU02Y3NRbEtmSXpISmQyQ0RxQzNJUWJFL24vWVVmT3Z5OHdZPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxCWZpbGU6b2ZmbGluZS1maXh0dXJlCXZlcnNpb246MS4yLjMKcGZRdnY5RWQ3V2ZOOHVub2RMUFJPWXBleWNidy9ERk5Zd3ltNmdkVHNRS2tPWnRUTHFpL09BOENOTUY5NTRENHRvS2NjNUp6dGE2MVlSWjhPQzNQQ2c9PQo=";
const WINDOWS_SIG_124: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG9mZmxpbmUgdGVzdCBzaWduYXR1cmUKUlVSTVRsTlVSVk5VTWVrbW5xT3o3emdidjBQQTlTdWxJVnlialJvRGtMT0F3L0VJNE9KWmxTOGkxWlNJZ2dDMzFCTTJORXJPOERuNzM1Wi96c3dBRGM2emkvenBsMktGM1FvPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxCWZpbGU6b2ZmbGluZS1maXh0dXJlCXZlcnNpb246MS4yLjQKNUNtUjNTei82a2JmNVRQMnJOMVhEYXpTdU5SRmkxTGl5OVl5UDRRRVlINnhGeDdiS09MYm1EUU4yMktBV1pIbVROSStYU1FjckhxSTVqeEdZUmRwQmc9PQo=";
const MAC_SIG_124: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG9mZmxpbmUgdGVzdCBzaWduYXR1cmUKUlVSTVRsTlVSVk5VTVdoZjFqTkR4QlZ2VzluRzVlTy9TM2dRb3gvWnRhOFJsVURCc2xlcHdKdXl4RlZTei9JR2VSRXIxS1p1bDZLd1hWVnF1RXE5Vkh0MFJpWjZpYm8zdGdzPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxCWZpbGU6b2ZmbGluZS1maXh0dXJlCXZlcnNpb246MS4yLjQKdjdlaUtWMklCTkxIYk9TVmVoQnc1a08rMjNMdjZMM2llNGJ3Z2NIem9jdU5VTldNTTJVaDE0SUNSTTdLZngweGJJVFJjVmRXQStDRnpzUzVzekFYRFE9PQo=";
const WRONG_VERSION_SIG: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG9mZmxpbmUgdGVzdCBzaWduYXR1cmUKUlVSTVRsTlVSVk5VTVdJaGJ2MEZrWHNiNDZwcXIrOXIyclhOSGtUcG9Fb0wzU2dnQzJ0SnpHd3Z6bXRnN2h0QUJNQkptZ2VCSXRRQndsNjhwYmxXL1UrVzRhdHFTRkpVS0FFPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxCWZpbGU6b2ZmbGluZS1maXh0dXJlCXZlcnNpb246OS45LjkKWW1ZRHN2T1R0bUVEUFArWlJrNXk2dXpodWwybDdLVEhsa1pGSWJSV1NYbXdzQ2FIS1FwWjlrUUpXWnM4ODRNd3hDMGZ4c05kU2UwSVJWWkRhVEplQmc9PQo=";

#[derive(Default)]
struct Upstream {
    files: Mutex<BTreeMap<String, Vec<u8>>>,
    requests: Mutex<Vec<String>>,
    redirects: Mutex<BTreeMap<String, String>>,
}

impl Upstream {
    fn release(&self, version: &str) {
        let mut files = BTreeMap::new();
        let windows = format!("TokenLens_{version}_x64-setup.exe");
        let mac = format!("TokenLens_{version}_aarch64.app.tar.gz");
        let dmg = format!("TokenLens_{version}_aarch64.dmg");
        let (windows_sig, mac_sig) = match version {
            "1.2.3" => (WINDOWS_SIG_123, MAC_SIG_123),
            "1.2.4" => (WINDOWS_SIG_124, MAC_SIG_124),
            _ => panic!("no fixture signature for this version"),
        };
        let manifest = serde_json::to_vec_pretty(&json!({
            "version": version,
            "notes": "Offline signed fixture",
            "pub_date": "2026-11-16T00:00:00Z",
            "platforms": {
                "windows-x86_64": {"signature": windows_sig, "url": github(version, &windows)},
                "darwin-aarch64": {"signature": mac_sig, "url": github(version, &mac)}
            }
        }))
        .expect("fixture manifest");
        for (name, bytes) in [
            (
                windows.clone(),
                format!("offline windows updater fixture {version}\n").into_bytes(),
            ),
            (
                mac.clone(),
                format!("offline macos updater fixture {version}\n").into_bytes(),
            ),
            (format!("{windows}.sig"), signature_file(windows_sig)),
            (format!("{mac}.sig"), signature_file(mac_sig)),
            (
                dmg,
                format!("offline macos installer fixture {version}\n").into_bytes(),
            ),
        ] {
            files.insert(name, bytes);
        }
        let mut sums = String::new();
        for (name, bytes) in &files {
            sums.push_str(&format!("{:x}  {name}\n", Sha256::digest(bytes)));
        }
        sums.push_str(&format!("{:x}  latest.json\n", Sha256::digest(&manifest)));
        files.insert("SHA256SUMS.txt".to_owned(), sums.into_bytes());
        let mut upstream = self.files.lock().expect("upstream");
        upstream.insert(UPSTREAM_MANIFEST.to_owned(), manifest.clone());
        for (name, bytes) in files {
            upstream.insert(github(version, &name), bytes);
        }
        upstream.insert(github(version, "latest.json"), manifest);
    }

    fn replace(&self, url: &str, bytes: Vec<u8>) {
        self.files
            .lock()
            .expect("upstream")
            .insert(url.to_owned(), bytes);
    }
}

impl ReleaseSource for Upstream {
    fn get(&self, url: &str, limit: usize) -> Result<Vec<u8>, MirrorError> {
        self.requests.lock().expect("requests").push(url.to_owned());
        if let Some(location) = self.redirects.lock().expect("redirects").get(url) {
            return match release_mirror::checked_redirect(url, location) {
                Ok(_) => Err(MirrorError {
                    kind: ErrorKind::Response,
                }),
                Err(error) => Err(error),
            };
        }
        if !url.starts_with("https://github.com/Dawn-4ever1/TokenLens/releases/") {
            return Err(MirrorError {
                kind: ErrorKind::Url,
            });
        }
        let bytes = self
            .files
            .lock()
            .expect("upstream")
            .get(url)
            .cloned()
            .ok_or(MirrorError {
                kind: ErrorKind::Response,
            })?;
        if bytes.len() > limit {
            return Err(MirrorError {
                kind: ErrorKind::Response,
            });
        }
        Ok(bytes)
    }
}

fn github(version: &str, filename: &str) -> String {
    format!("https://github.com/Dawn-4ever1/TokenLens/releases/download/v{version}/{filename}")
}

fn signature_file(encoded: &str) -> Vec<u8> {
    // Tauri uploads the base64 signature as a literal `.sig` text asset.
    encoded.as_bytes().to_vec()
}

fn open_mirror(root: &std::path::Path, upstream: Arc<Upstream>) -> Arc<ReleaseMirror> {
    ReleaseMirror::open_for_test(
        root.join("mirror"),
        "https://downloads.example.test",
        PUBLIC_KEY,
        upstream,
    )
    .expect("open mirror")
}

fn upstream() -> Arc<Upstream> {
    let upstream = Arc::new(Upstream::default());
    upstream.release("1.2.3");
    upstream
}

#[tokio::test]
async fn complete_signed_release_publishes_all_seven_assets_and_survives_restart() {
    let root = tempfile::tempdir().expect("tempdir");
    let upstream = upstream();
    let mirror = open_mirror(root.path(), Arc::clone(&upstream));
    mirror.sync().await.expect("sync valid release");
    let snapshot = mirror.current().expect("published");
    assert_eq!(snapshot.version(), "1.2.3");
    for name in [
        "latest.json",
        "SHA256SUMS.txt",
        "TokenLens_1.2.3_x64-setup.exe",
        "TokenLens_1.2.3_x64-setup.exe.sig",
        "TokenLens_1.2.3_aarch64.app.tar.gz",
        "TokenLens_1.2.3_aarch64.app.tar.gz.sig",
        "TokenLens_1.2.3_aarch64.dmg",
    ] {
        assert!(snapshot.file_path(name).is_some(), "{name}");
    }
    let manifest: Value = serde_json::from_slice(snapshot.manifest()).expect("manifest");
    assert_eq!(
        manifest["platforms"]["windows-x86_64"]["url"],
        "https://downloads.example.test/downloads/v1.2.3/TokenLens_1.2.3_x64-setup.exe"
    );
    assert_eq!(
        manifest["platforms"]["darwin-aarch64"]["url"],
        "https://downloads.example.test/downloads/v1.2.3/TokenLens_1.2.3_aarch64.app.tar.gz"
    );
    assert_eq!(
        manifest["platforms"]["windows-x86_64"]["signature"],
        WINDOWS_SIG_123
    );
    assert!(root.path().join("mirror/current.json").is_file());
    assert!(
        root.path()
            .join("mirror/versions/1.2.3/TokenLens_1.2.3_aarch64.dmg")
            .is_file()
    );
    drop(mirror);
    let reopened = open_mirror(root.path(), Arc::clone(&upstream));
    assert_eq!(reopened.current().expect("restart").version(), "1.2.3");
    drop(reopened);
    std::fs::remove_file(
        root.path()
            .join("mirror/versions/1.2.3/TokenLens_1.2.3_aarch64.dmg"),
    )
    .expect("remove required asset");
    let restored = ReleaseMirror::open_for_test(
        root.path().join("mirror"),
        "https://downloads.example.test",
        PUBLIC_KEY,
        upstream,
    );
    match restored {
        Err(error) => assert_eq!(error.kind, ErrorKind::Io),
        Ok(_) => panic!("partial release cannot be restored"),
    }
}

#[tokio::test]
async fn real_release_public_key_accepts_an_existing_offline_release() {
    let Some(directory) = std::env::var_os("TOKENLENS_TEST_RELEASE_DIR") else {
        // The primary suite never depends on developer-machine artifacts.
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    let mut manifest = std::fs::read(directory.join("latest.json")).expect("real manifest");
    if !manifest.ends_with(b"\n") {
        manifest.push(b'\n');
    }
    let version: Value = serde_json::from_slice(&manifest).expect("real version");
    let version = version["version"].as_str().expect("version");
    let upstream = Arc::new(Upstream::default());
    let asset_names = [
        "SHA256SUMS.txt".to_owned(),
        format!("TokenLens_{version}_x64-setup.exe"),
        format!("TokenLens_{version}_x64-setup.exe.sig"),
        format!("TokenLens_{version}_aarch64.app.tar.gz"),
        format!("TokenLens_{version}_aarch64.app.tar.gz.sig"),
        format!("TokenLens_{version}_aarch64.dmg"),
    ];
    {
        let mut files = upstream.files.lock().expect("files");
        files.insert(UPSTREAM_MANIFEST.to_owned(), manifest.clone());
        files.insert(github(version, "latest.json"), manifest.clone());
        for name in asset_names {
            let name: &str = if name == "SHA256SUMS.txt"
                && directory.join("original-SHA256SUMS.txt").is_file()
            {
                "original-SHA256SUMS.txt"
            } else {
                name.as_str()
            };
            files.insert(
                github(version, name),
                std::fs::read(directory.join(name)).expect("real asset"),
            );
        }
    }
    let root = tempfile::tempdir().expect("tempdir");
    let mirror = ReleaseMirror::open_for_test(
        root.path().join("mirror"),
        release_mirror::DEFAULT_PUBLIC_BASE,
        release_mirror::UPDATER_PUBLIC_KEY,
        upstream,
    )
    .expect("real key");
    assert_eq!(
        mirror
            .sync()
            .await
            .expect("real release signature")
            .to_string(),
        version
    );
}

#[tokio::test]
async fn checksum_signature_and_version_binding_fail_closed() {
    for failure in ["checksum", "signature", "signed_version"] {
        let root = tempfile::tempdir().expect("tempdir");
        let upstream = upstream();
        match failure {
            "checksum" => upstream.replace(
                &github("1.2.3", "TokenLens_1.2.3_aarch64.dmg"),
                b"tampered".to_vec(),
            ),
            "signature" => upstream.replace(
                &github("1.2.3", "TokenLens_1.2.3_x64-setup.exe.sig"),
                signature_file(MAC_SIG_123),
            ),
            "signed_version" => {
                upstream.replace(
                    UPSTREAM_MANIFEST,
                    manifest_with_windows_signature(WRONG_VERSION_SIG),
                );
                // The .sig and manifest agree, so only the authenticated
                // trusted-comment version check can reject this candidate.
                upstream.replace(
                    &github("1.2.3", "TokenLens_1.2.3_x64-setup.exe.sig"),
                    signature_file(WRONG_VERSION_SIG),
                );
                let files = upstream.files.lock().expect("upstream").clone();
                let mut sums = String::new();
                for name in [
                    "TokenLens_1.2.3_aarch64.app.tar.gz",
                    "TokenLens_1.2.3_aarch64.app.tar.gz.sig",
                    "TokenLens_1.2.3_aarch64.dmg",
                    "TokenLens_1.2.3_x64-setup.exe",
                    "TokenLens_1.2.3_x64-setup.exe.sig",
                ] {
                    let url = github("1.2.3", name);
                    sums.push_str(&format!("{:x}  {name}\n", Sha256::digest(&files[&url])));
                }
                let manifest = files[UPSTREAM_MANIFEST].clone();
                sums.push_str(&format!("{:x}  latest.json\n", Sha256::digest(manifest)));
                upstream.replace(&github("1.2.3", "SHA256SUMS.txt"), sums.into_bytes());
            }
            _ => unreachable!(),
        }
        let mirror = open_mirror(root.path(), upstream);
        let error = mirror.sync().await.expect_err(failure);
        let expected = match failure {
            "checksum" => ErrorKind::Checksum,
            "signature" => ErrorKind::Signature,
            "signed_version" => ErrorKind::SignedVersion,
            _ => unreachable!(),
        };
        assert_eq!(error.kind, expected, "{failure}");
        assert!(mirror.current().is_none());
        assert!(!root.path().join("mirror/current.json").exists());
    }
}

fn manifest_with_windows_signature(signature: &str) -> Vec<u8> {
    let mut manifest: Value = serde_json::from_slice(&upstream_manifest()).expect("manifest");
    manifest["platforms"]["windows-x86_64"]["signature"] = Value::String(signature.to_owned());
    serde_json::to_vec_pretty(&manifest).expect("manifest")
}

fn upstream_manifest() -> Vec<u8> {
    serde_json::to_vec_pretty(&json!({
        "version": "1.2.3",
        "notes": "Offline signed fixture",
        "pub_date": "2026-11-16T00:00:00Z",
        "platforms": {
            "windows-x86_64": {"signature": WINDOWS_SIG_123, "url": github("1.2.3", "TokenLens_1.2.3_x64-setup.exe")},
            "darwin-aarch64": {"signature": MAC_SIG_123, "url": github("1.2.3", "TokenLens_1.2.3_aarch64.app.tar.gz")}
        }
    })).expect("manifest")
}

#[tokio::test]
async fn manifest_cannot_choose_an_arbitrary_host_or_asset() {
    for url in [
        "http://127.0.0.1/release.exe",
        "https://localhost/release.exe",
        "https://github.com.evil.example/Dawn-4ever1/TokenLens/releases/download/v1.2.3/TokenLens_1.2.3_x64-setup.exe",
        &github("1.2.3", "other.exe"),
        "https://release-assets.githubusercontent.com/asset",
    ] {
        let root = tempfile::tempdir().expect("tempdir");
        let upstream = upstream();
        let mut manifest: Value = serde_json::from_slice(&upstream_manifest()).expect("manifest");
        manifest["platforms"]["windows-x86_64"]["url"] = Value::String(url.to_owned());
        upstream.replace(
            UPSTREAM_MANIFEST,
            serde_json::to_vec(&manifest).expect("manifest"),
        );
        let mirror = open_mirror(root.path(), upstream);
        assert_eq!(
            mirror.sync().await.expect_err("bad url").kind,
            ErrorKind::Url
        );
    }
}

#[tokio::test]
async fn redirects_to_private_local_or_unrelated_hosts_are_rejected() {
    for location in [
        "http://127.0.0.1/private",
        "https://localhost/private",
        "https://github.com/other/repository/releases/download/v1.2.3/latest.json",
        "https://release-assets.githubusercontent.com.evil.example/asset?query=value",
    ] {
        let root = tempfile::tempdir().expect("tempdir");
        let upstream = upstream();
        upstream
            .redirects
            .lock()
            .expect("redirect")
            .insert(UPSTREAM_MANIFEST.to_owned(), location.to_owned());
        let mirror = open_mirror(root.path(), upstream);
        assert_eq!(
            mirror.sync().await.expect_err("redirect").kind,
            ErrorKind::Url
        );
        assert!(mirror.current().is_none());
    }
}

#[test]
fn real_github_asset_redirects_keep_their_signed_query() {
    let signed = "https://release-assets.githubusercontent.com/github-production-release-asset/1/2?sp=r&sv=2018-11-09&sig=abc";
    assert!(release_mirror::checked_redirect(UPSTREAM_MANIFEST, signed).is_ok());
    assert!(
        release_mirror::checked_redirect(
            UPSTREAM_MANIFEST,
            "https://release-assets.githubusercontent.com/asset"
        )
        .is_err()
    );
}

#[tokio::test]
async fn stale_manifest_and_bad_upgrade_keep_serving_the_old_complete_release() {
    let root = tempfile::tempdir().expect("tempdir");
    let upstream = upstream();
    let mirror = open_mirror(root.path(), Arc::clone(&upstream));
    mirror.sync().await.expect("first release");
    upstream.release("1.2.4");
    mirror.sync().await.expect("new release");
    assert_eq!(mirror.current().expect("new current").version(), "1.2.4");
    upstream.release("1.2.3");
    assert_eq!(
        mirror.sync().await.expect_err("stale").kind,
        ErrorKind::Stale
    );
    assert_eq!(mirror.current().expect("still new").version(), "1.2.4");
}

#[tokio::test]
async fn corrupt_newer_release_keeps_serving_the_old_version() {
    let root = tempfile::tempdir().expect("tempdir");
    let upstream = upstream();
    let mirror = open_mirror(root.path(), Arc::clone(&upstream));
    mirror.sync().await.expect("first release");
    upstream.release("1.2.4");
    upstream.replace(
        &github("1.2.4", "TokenLens_1.2.4_aarch64.dmg"),
        b"new candidate is incomplete".to_vec(),
    );
    assert_eq!(
        mirror.sync().await.expect_err("bad upgrade").kind,
        ErrorKind::Checksum
    );
    assert_eq!(
        mirror.current().expect("old after failure").version(),
        "1.2.3"
    );
}

#[tokio::test]
async fn http_routes_support_head_fixed_urls_and_preserve_authentication() {
    let root = tempfile::tempdir().expect("tempdir");
    let upstream = upstream();
    let mirror = open_mirror(root.path(), Arc::clone(&upstream));
    mirror.sync().await.expect("sync");
    let harness = Harness::start_with_mirror(Arc::clone(&mirror)).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");
    let response = client
        .get(format!("{}/downloads/latest.json", harness.base))
        .header("host", "attacker.invalid")
        .send()
        .await
        .expect("manifest");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.text().await.expect("manifest response");
    let health = client.get(format!("{}/healthz", harness.base)).send().await;
    assert_eq!(
        status,
        StatusCode::OK,
        "mirror body={body:?} headers={headers:?} health={health:?}"
    );
    assert_eq!(
        headers["cache-control"],
        "public, max-age=60, must-revalidate"
    );
    let manifest: Value = serde_json::from_str(&body).expect("manifest");
    assert!(
        manifest["platforms"]["windows-x86_64"]["url"]
            .as_str()
            .expect("url")
            .starts_with("https://downloads.example.test/")
    );
    for (platform, filename) in [
        ("windows", "TokenLens_1.2.3_x64-setup.exe"),
        ("macos", "TokenLens_1.2.3_aarch64.dmg"),
    ] {
        let response = client
            .get(format!("{}/downloads/latest/{platform}", harness.base))
            .send()
            .await
            .expect("latest");
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers()["location"],
            format!("https://downloads.example.test/downloads/v1.2.3/{filename}?source=website")
                .as_str()
        );
        assert_eq!(response.headers()["cache-control"], "no-cache");
    }
    let url = format!(
        "{}/downloads/v1.2.3/TokenLens_1.2.3_x64-setup.exe",
        harness.base
    );
    let response = client.get(&url).send().await.expect("file");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    assert_eq!(
        response.headers()["content-disposition"],
        "attachment; filename=\"TokenLens_1.2.3_x64-setup.exe\""
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        response.headers()["cache-control"],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        response.bytes().await.expect("body").as_ref(),
        b"offline windows updater fixture 1.2.3\n"
    );
    let head = client.head(&url).send().await.expect("head");
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(head.headers()["content-length"], "38");
    assert!(head.bytes().await.expect("head body").is_empty());
    for path in [
        "/downloads/v1.2.3/not-allowed.txt",
        "/downloads/v1.2.3/..%2F..%2Fserver.sqlite",
        "/downloads/v9.9.9/TokenLens_1.2.3_x64-setup.exe",
        "/downloads/v1.2.3/%2e%2e%2f%2e%2e%2fserver.sqlite",
    ] {
        assert_eq!(
            client
                .get(format!("{}{path}", harness.base))
                .send()
                .await
                .expect("bad path")
                .status(),
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(
        client.post(&url).send().await.expect("post").status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        client
            .get(format!("{}/api/dashboard/summary", harness.base))
            .send()
            .await
            .expect("protected")
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn website_download_clicks_and_files_are_counted_without_updater_traffic() {
    let root = tempfile::tempdir().expect("tempdir");
    let upstream = upstream();
    let mirror = open_mirror(root.path(), Arc::clone(&upstream));
    mirror.sync().await.expect("sync");
    let harness = Harness::start_with_mirror_and_stats(mirror, Some("test-stats-token")).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client");

    let response = client
        .get(format!("{}/downloads/latest/windows", harness.base))
        .send()
        .await
        .expect("click");
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response.headers()["location"]
        .to_str()
        .expect("location")
        .to_owned();
    assert!(location.ends_with("?source=website"), "{location}");
    harness.state.flush_site_metrics().await;
    assert_eq!(
        harness
            .count("SELECT count FROM site_daily_counts WHERE metric = 'download_click_windows'"),
        1
    );

    // HEAD checks and direct updater downloads must not look like website
    // downloads; only the redirect marked with source=website is counted.
    assert_eq!(
        client
            .head(format!("{}/downloads/latest/windows", harness.base))
            .send()
            .await
            .expect("head")
            .status(),
        StatusCode::FOUND
    );
    harness.state.flush_site_metrics().await;
    assert_eq!(
        harness
            .count("SELECT count FROM site_daily_counts WHERE metric = 'download_click_windows'"),
        1
    );

    let path = location
        .split("downloads.example.test")
        .nth(1)
        .expect("mirror path");
    assert_eq!(
        client
            .get(format!("{}{}", harness.base, path))
            .send()
            .await
            .expect("file")
            .status(),
        StatusCode::OK
    );
    harness.state.flush_site_metrics().await;
    assert_eq!(
        harness.count("SELECT count FROM site_daily_counts WHERE metric = 'download_file_windows'"),
        1
    );
    assert_eq!(
        client
            .get(format!(
                "{}/downloads/v1.2.3/TokenLens_1.2.3_x64-setup.exe",
                harness.base
            ))
            .send()
            .await
            .expect("updater file")
            .status(),
        StatusCode::OK
    );
    harness.state.flush_site_metrics().await;
    assert_eq!(
        harness.count("SELECT count FROM site_daily_counts WHERE metric = 'download_file_windows'"),
        1
    );

    let page = client
        .get(format!("{}/stats/test-stats-token", harness.base))
        .send()
        .await
        .expect("stats")
        .text()
        .await
        .expect("body");
    assert!(page.contains("官网访问统计"));
    assert!(page.contains("<strong>1</strong>"), "{page}");
}

#[tokio::test]
async fn unavailable_or_unconfigured_mirror_never_looks_like_no_update() {
    let harness = Harness::start().await;
    for path in [
        "/downloads/latest.json",
        "/downloads/latest/windows",
        "/downloads/v1.2.3/latest.json",
    ] {
        let response = harness
            .http
            .get(format!("{}{path}", harness.base))
            .send()
            .await
            .expect("unavailable");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.json::<Value>().await.expect("error")["error"],
            "release_mirror_unavailable"
        );
    }
}

#[tokio::test]
async fn corrupted_served_file_is_not_returned_as_a_verified_download() {
    let root = tempfile::tempdir().expect("tempdir");
    let upstream = upstream();
    let mirror = open_mirror(root.path(), Arc::clone(&upstream));
    mirror.sync().await.expect("sync");
    let harness = Harness::start_with_mirror(mirror).await;
    let path = root
        .path()
        .join("mirror/versions/1.2.3/TokenLens_1.2.3_aarch64.dmg");
    std::fs::write(path, b"tampered").expect("tamper");
    let response = harness
        .http
        .get(format!(
            "{}/downloads/v1.2.3/TokenLens_1.2.3_aarch64.dmg",
            harness.base
        ))
        .send()
        .await
        .expect("download");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn download_asset_supports_resumable_byte_ranges_and_prunes_older_versions() {
    let root = tempfile::tempdir().expect("tempdir");
    // Pre-create two older historical version folders before publishing 1.2.3 -> 1.2.4.
    let versions_dir = root.path().join("mirror/versions");
    std::fs::create_dir_all(versions_dir.join("0.9.0")).expect("old 0.9.0");
    std::fs::create_dir_all(versions_dir.join("1.0.0")).expect("old 1.0.0");

    let upstream = upstream();
    let mirror = open_mirror(root.path(), Arc::clone(&upstream));
    mirror.sync().await.expect("1.2.3");
    assert!(versions_dir.join("1.2.3").exists());
    assert!(versions_dir.join("1.0.0").exists(), "one fallback kept");
    assert!(
        !versions_dir.join("0.9.0").exists(),
        "older version pruned on publish"
    );

    upstream.release("1.2.4");
    mirror.sync().await.expect("1.2.4");
    assert!(versions_dir.join("1.2.4").exists());
    assert!(versions_dir.join("1.2.3").exists());
    assert!(
        !versions_dir.join("1.0.0").exists(),
        "superseded grand-parent pruned"
    );

    let harness =
        Harness::start_with_mirror_and_stats(Arc::clone(&mirror), Some("range-stats-token")).await;
    let url = format!(
        "{}/downloads/v1.2.4/TokenLens_1.2.4_x64-setup.exe?source=website",
        harness.base
    );

    // First 8 bytes -> 206 Partial Content, counts download once (start == 0).
    let first = harness
        .http
        .get(&url)
        .header("range", "bytes=0-7")
        .send()
        .await
        .expect("range 0-7");
    assert_eq!(first.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(first.headers()["accept-ranges"], "bytes");
    assert_eq!(first.headers()["content-range"], "bytes 0-7/38");
    assert_eq!(first.headers()["content-length"], "8");
    assert_eq!(first.bytes().await.expect("bytes").as_ref(), b"offline ");

    // Resume from byte 8 -> 206 Partial Content, must NOT double-count website download!
    let resumed = harness
        .http
        .get(&url)
        .header("range", "bytes=8-")
        .send()
        .await
        .expect("range 8-");
    assert_eq!(resumed.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(resumed.headers()["content-range"], "bytes 8-37/38");
    assert_eq!(
        resumed.bytes().await.expect("bytes").as_ref(),
        b"windows updater fixture 1.2.4\n"
    );
    harness.state.flush_site_metrics().await;
    assert_eq!(
        harness.count("SELECT count FROM site_daily_counts WHERE metric = 'download_file_windows'"),
        1
    );

    // Out-of-bounds range -> 416 Range Not Satisfiable.
    let bad_range = harness
        .http
        .get(&url)
        .header("range", "bytes=999-1000")
        .send()
        .await
        .expect("bad range");
    assert_eq!(bad_range.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(bad_range.headers()["content-range"], "bytes */38");
}
