//! Self-update of the AppImage: reads the latest GitHub release, picks the AppImage, its checksum list and the
//! list's minisign signature, verifies the download. The caller (the app) decides where files go and when to
//! restart.

use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::Duration;

pub const LATEST_URL: &str = "https://api.github.com/repos/williancima/headroom/releases/latest";
pub const APPIMAGE_NAME: &str = "Headroom-x86_64.AppImage";
pub const SUMS_NAME: &str = "SHA256SUMS";
pub const SIG_NAME: &str = "SHA256SUMS.minisig";
/// The release signing key. The release workflow signs `SHA256SUMS` with its secret half (a repository secret).
const PUBLIC_KEY: &str = "RWSJvReAKvMzJnvQTzV973bN63TcYQnRX56PYHpYQz7OhDvR9EyYZMQL";

#[derive(Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Update {
    pub version: String,
    pub appimage_url: String,
    pub sums_url: String,
    pub sig_url: String,
}

/// `v1.2.3` or `1.2.3` as numbers. Pre-releases, build metadata and anything else are `None`.
fn parse_version(text: &str) -> Option<[u64; 3]> {
    let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
    let mut next = || parts.next().filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))?.parse().ok();
    let version = [next()?, next()?, next()?];
    parts.next().is_none().then_some(version)
}

/// The update to install when the release is newer than `current` and ships all three files. A release whose
/// signature is not uploaded yet is skipped until the next check.
pub fn pick(release: &Release, current: &str) -> Option<Update> {
    let newer = parse_version(&release.tag_name)? > parse_version(current)?;
    let url = |name: &str| release.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());
    newer.then_some(())?;
    Some(Update {
        version: release.tag_name.trim_start_matches('v').to_string(),
        appimage_url: url(APPIMAGE_NAME)?,
        sums_url: url(SUMS_NAME)?,
        sig_url: url(SIG_NAME)?,
    })
}

/// Checks that `sig` is a signature of `sums` by `key` and that its trusted comment is `headroom <version>`, so a
/// validly signed list of an older release cannot be served as a newer one.
fn verify_sums(sums: &[u8], sig: &str, key: &str, version: &str) -> Result<(), String> {
    let key = PublicKey::from_base64(key).map_err(|_| "the signing key is invalid")?;
    let sig = Signature::decode(sig).map_err(|_| "the checksum signature was not understood")?;
    key.verify(sums, &sig, false).map_err(|_| "the checksum list is not signed by Headroom")?;
    if sig.trusted_comment() != format!("headroom {version}") {
        return Err("the checksum signature is for another version".into());
    }
    Ok(())
}

/// The hex digest listed for `file` in `sha256sum` output (`<hex>  <name>`, name optionally `*`-prefixed).
pub fn checksum_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hex, name) = line.split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        (name == file && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex.to_ascii_lowercase())
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn client() -> Result<reqwest::Client, String> {
    crate::http::client_builder()
        .user_agent(concat!("headroom/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|_| "could not start the HTTP client".to_string())
}

/// GET `url`; the error names only the status, never the body.
pub async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let response = client()?.get(url).send().await.map_err(|e| if e.is_timeout() { "timed out".to_string() } else { "could not connect".to_string() })?;
    if !response.status().is_success() {
        return Err(format!("the server answered {}", response.status()));
    }
    response.bytes().await.map(|b| b.to_vec()).map_err(|_| "the download was interrupted".to_string())
}

pub async fn latest_release(url: &str) -> Result<Release, String> {
    serde_json::from_slice(&fetch(url).await?).map_err(|_| "the release answer was not understood".to_string())
}

/// Downloads and verifies the AppImage of `update`; the bytes are returned only when the checksum list carries
/// a valid signature and the AppImage matches it.
pub async fn download(update: &Update) -> Result<Vec<u8>, String> {
    download_with_key(update, PUBLIC_KEY).await
}

async fn download_with_key(update: &Update, key: &str) -> Result<Vec<u8>, String> {
    let sums = fetch(&update.sums_url).await?;
    let sig = String::from_utf8(fetch(&update.sig_url).await?).map_err(|_| "the checksum signature was not text".to_string())?;
    verify_sums(&sums, &sig, key, &update.version)?;
    let sums = String::from_utf8(sums).map_err(|_| "the checksum list was not text".to_string())?;
    let expected = checksum_for(&sums, APPIMAGE_NAME).ok_or("the checksum list has no entry for the AppImage")?;
    let bytes = fetch(&update.appimage_url).await?;
    if sha256_hex(&bytes) != expected {
        return Err("the download does not match its checksum".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, names: &[&str]) -> Release {
        Release { tag_name: tag.into(), assets: names.iter().map(|n| Asset { name: n.to_string(), browser_download_url: format!("https://x/{n}") }).collect() }
    }

    const BOTH: [&str; 3] = [APPIMAGE_NAME, SUMS_NAME, SIG_NAME];

    // Test key and a signature of `TEST_SUMS` with trusted comment "headroom 9.9.9", built with a fixed seed.
    const TEST_KEY: &str = "RWQIBwYFBAMCAQOhB7/zzhC+HXDdGOdLwJln5NYwm6UNXx3chmQSVTG4";
    const TEST_SUMS: &str = "b22b959cff871dc21603c713bce3cee20d02747014192f3b63331a9fbee6cecd  Headroom-x86_64.AppImage\n";
    const TEST_SIG: &str = "untrusted comment: test
RUQIBwYFBAMCAWuo84kWre0tESaOmqrbPDDxvfUevc5DsX+oBAYJ0ux1eew/fZzAKgy6KSSXsa7ebFWgfaNjP8WmdKcgFPJAhgE=
trusted comment: headroom 9.9.9
mIkh8qL+wUgk/vdoQtyEAuhjM52/o+YxcisYHSr7vaDvRBucCjZJmlgOX6vebDpyhdqFZ0rJ3c540aMMAkn+Aw==
";

    #[test]
    fn embedded_key_parses() {
        assert!(PublicKey::from_base64(PUBLIC_KEY).is_ok());
    }

    #[test]
    fn signature_must_match_list_key_and_version() {
        assert_eq!(verify_sums(TEST_SUMS.as_bytes(), TEST_SIG, TEST_KEY, "9.9.9"), Ok(()));
        let tampered = TEST_SUMS.replace("b22b", "0000");
        assert!(verify_sums(tampered.as_bytes(), TEST_SIG, TEST_KEY, "9.9.9").unwrap_err().contains("not signed"));
        assert!(verify_sums(TEST_SUMS.as_bytes(), TEST_SIG, PUBLIC_KEY, "9.9.9").unwrap_err().contains("not signed"));
        assert!(verify_sums(TEST_SUMS.as_bytes(), TEST_SIG, TEST_KEY, "9.9.10").unwrap_err().contains("another version"));
        let forged_comment = TEST_SIG.replace("headroom 9.9.9", "headroom 9.9.10");
        assert!(verify_sums(TEST_SUMS.as_bytes(), &forged_comment, TEST_KEY, "9.9.10").unwrap_err().contains("not signed"));
        assert!(verify_sums(TEST_SUMS.as_bytes(), "garbage", TEST_KEY, "9.9.9").unwrap_err().contains("not understood"));
    }

    #[test]
    fn legacy_signatures_are_refused() {
        // Same key, list and comment, but the legacy (non-prehashed) "Ed" algorithm.
        let legacy = "untrusted comment: test
RWQIBwYFBAMCAY+DJv/Qb26vE0o5IyX9K5EF0W45J54oWCRz9A50LuCTHCSnUa32t5F4GmpQp4nAyq+Ri80VY2SY9uFUL1vSYQg=
trusted comment: headroom 9.9.9
X9lxHEdcNsRb8/ZXvtPX34w9SMQ6fb92+WyDFC5zHMbb+8QWhLyUaQ/Z3sfkpYT0tg4SVJQu6whctcfF01XzDQ==
";
        assert!(verify_sums(TEST_SUMS.as_bytes(), legacy, TEST_KEY, "9.9.9").unwrap_err().contains("not signed"));
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(pick(&release("v0.10.0", &BOTH), "0.9.9").is_some());
        assert!(pick(&release("v1.0.0", &BOTH), "0.99.99").is_some());
        assert!(pick(&release("v0.1.1", &BOTH), "0.1.0").is_some());
        assert!(pick(&release("v0.1.0", &BOTH), "0.1.0").is_none());
        assert!(pick(&release("v0.9.0", &BOTH), "0.10.0").is_none());
    }

    #[test]
    fn prereleases_and_junk_are_ignored() {
        for tag in ["v2.0.0-rc1", "v2.0", "v2.0.0.1", "latest", "v2.x.0", "v2..0", ""] {
            assert!(pick(&release(tag, &BOTH), "0.1.0").is_none(), "{tag}");
        }
    }

    #[test]
    fn needs_all_assets() {
        let update = pick(&release("v0.2.0", &BOTH), "0.1.0").unwrap();
        let expected = Update {
            version: "0.2.0".into(),
            appimage_url: format!("https://x/{APPIMAGE_NAME}"),
            sums_url: "https://x/SHA256SUMS".into(),
            sig_url: "https://x/SHA256SUMS.minisig".into(),
        };
        assert_eq!(update, expected);
        assert!(pick(&release("v0.2.0", &[APPIMAGE_NAME]), "0.1.0").is_none());
        assert!(pick(&release("v0.2.0", &[SUMS_NAME, "headroom.deb"]), "0.1.0").is_none());
        assert!(pick(&release("v0.2.0", &[APPIMAGE_NAME, SUMS_NAME]), "0.1.0").is_none());
    }

    #[test]
    fn checksum_lines_parse() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  headroom_0.2.0_amd64.deb\n{b} *{APPIMAGE_NAME}\nnot a line\n");
        assert_eq!(checksum_for(&sums, APPIMAGE_NAME), Some("b".repeat(64)));
        assert_eq!(checksum_for(&sums, "headroom_0.2.0_amd64.deb"), Some(a));
        assert_eq!(checksum_for(&sums, "other"), None);
        assert_eq!(checksum_for("abc  Headroom-x86_64.AppImage", APPIMAGE_NAME), None);
    }

    #[test]
    fn sha256_of_known_input() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[tokio::test]
    async fn download_verifies_signature_and_checksum() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        let body = b"appimage bytes".to_vec();
        assert_eq!(TEST_SUMS, format!("{}  {APPIMAGE_NAME}\n", sha256_hex(&body)));
        let serve = |at: &str, bytes: Vec<u8>| Mock::given(method("GET")).and(path(at.to_string())).respond_with(ResponseTemplate::new(200).set_body_bytes(bytes));
        serve("/image", body.clone()).mount(&server).await;
        serve("/other-image", b"something else".to_vec()).mount(&server).await;
        serve("/sums", TEST_SUMS.into()).mount(&server).await;
        serve("/sig", TEST_SIG.into()).mount(&server).await;
        serve("/unsigned-sums", format!("{}  {APPIMAGE_NAME}\n", sha256_hex(b"something else")).into()).mount(&server).await;
        let update = |image: &str, sums: &str, sig: &str| Update {
            version: "9.9.9".into(),
            appimage_url: format!("{}/{image}", server.uri()),
            sums_url: format!("{}/{sums}", server.uri()),
            sig_url: format!("{}/{sig}", server.uri()),
        };
        assert_eq!(download_with_key(&update("image", "sums", "sig"), TEST_KEY).await.unwrap(), body);
        assert!(download_with_key(&update("other-image", "sums", "sig"), TEST_KEY).await.unwrap_err().contains("checksum"));
        assert!(download_with_key(&update("other-image", "unsigned-sums", "sig"), TEST_KEY).await.unwrap_err().contains("not signed"));
        assert!(download_with_key(&update("image", "sums", "missing"), TEST_KEY).await.unwrap_err().contains("404"));
        assert!(download(&update("image", "sums", "sig")).await.unwrap_err().contains("not signed"));
    }
}
