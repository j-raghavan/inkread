//! `inkread-update` (ADR-INKREAD-0014): the **decision** half of the in-app self-updater.
//!
//! inkread is sideloaded onto GMS-less Supernote devices, so there is no store to push updates. The
//! Android shell fetches the project's GitHub `releases/latest` payload and hands the JSON to this
//! crate, which decides whether that release is a newer build than the installed one and, if so,
//! surfaces the `.apk` + `.apk.sha256` asset URLs the shell should download. Everything here is
//! **pure**: no network, no clock, no vendor/host string (IR-7) — the shell owns the URL, the
//! download, the integrity checks, and the install.
//!
//! The public surface is a single string-in/string-out function, [`decide`], matching the
//! `parse_feed_json` contract `inkread-daily` exposes over the same JNI bridge.

use semver::Version;
use serde::{Deserialize, Serialize};

/// Decide whether `release_json` (a GitHub `releases/latest` payload) is a newer build than
/// `installed_version` (the app's `BuildConfig.VERSION_NAME`, e.g. `"0.3.0"`).
///
/// Returns the decision as a JSON string (see [`Decision`]). Any unusable input — malformed JSON, an
/// unparseable tag, a tag that is not strictly newer, or a release with no `.apk` asset — yields
/// `{"updateAvailable":false}` rather than an error (RR21-FR3: junk in → benign out).
pub fn decide(installed_version: &str, release_json: &str) -> String {
    let decision = match serde_json::from_str::<Release>(release_json) {
        Ok(release) => evaluate(installed_version, &release),
        Err(_) => Decision::none(),
    };
    // Serializing a plain struct of owned strings/bools cannot fail; fall back defensively anyway.
    serde_json::to_string(&decision).unwrap_or_else(|_| Decision::NONE_JSON.to_string())
}

/// Pure core of [`decide`] over a parsed [`Release`] — the host-tested seam.
fn evaluate(installed_version: &str, release: &Release) -> Decision {
    // A draft release is never published to users; ignore it defensively even though
    // `releases/latest` excludes drafts on the GitHub side.
    if release.draft {
        return Decision::none();
    }

    let installed = match parse_version(installed_version) {
        Some(v) => v,
        // If we cannot parse our own installed version we cannot safely compare — decline rather
        // than risk nagging with a wrong-direction "update".
        None => return Decision::none(),
    };
    let candidate = match parse_version(&release.tag_name) {
        Some(v) => v,
        None => return Decision::none(),
    };
    if candidate <= installed {
        return Decision::none();
    }

    // An update is only actionable if there is an APK to install. The `.apk.sha256` checksum is
    // optional (signer-pin verification still gates the install if it is absent).
    let apk_url = match release.assets.iter().find(|a| is_apk(&a.name)) {
        Some(asset) => asset.browser_download_url.clone(),
        None => return Decision::none(),
    };
    let sha256_url = release
        .assets
        .iter()
        .find(|a| is_sha256(&a.name))
        .map(|a| a.browser_download_url.clone())
        .unwrap_or_default();

    Decision {
        update_available: true,
        version: candidate.to_string(),
        notes: release.body.clone().unwrap_or_default(),
        apk_url,
        sha256_url,
    }
}

/// Parse a release tag or installed version into a [`Version`], tolerating a single leading `v`/`V`
/// (the release CI tags `v*`, the installed `VERSION_NAME` is the tag minus that `v`).
fn parse_version(raw: &str) -> Option<Version> {
    let trimmed = raw.trim();
    let body = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    Version::parse(body).ok()
}

/// An installable APK asset — anything ending `.apk` (and, by construction, not `.apk.sha256`).
fn is_apk(name: &str) -> bool {
    name.ends_with(".apk")
}

/// The published checksum sidecar (`<apk>.sha256`, as `sha256sum` writes it).
fn is_sha256(name: &str) -> bool {
    name.ends_with(".sha256")
}

/// The shell-facing decision, serialized to the JSON contract in ADR-INKREAD-0014 Decision 2.
#[derive(Debug, Serialize, PartialEq, Eq)]
struct Decision {
    #[serde(rename = "updateAvailable")]
    update_available: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    version: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    notes: String,
    #[serde(rename = "apkUrl", skip_serializing_if = "String::is_empty")]
    apk_url: String,
    #[serde(rename = "sha256Url", skip_serializing_if = "String::is_empty")]
    sha256_url: String,
}

impl Decision {
    const NONE_JSON: &'static str = "{\"updateAvailable\":false}";

    /// The "no update" decision — the single benign result for every unusable input.
    fn none() -> Self {
        Decision {
            update_available: false,
            version: String::new(),
            notes: String::new(),
            apk_url: String::new(),
            sha256_url: String::new(),
        }
    }
}

/// The slice of the GitHub `releases/latest` payload the decision needs. Unknown fields are ignored.
#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// One release asset (the attached APK and its checksum file).
#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic `releases/latest` payload with both assets, parameterized by tag.
    fn release_json(tag: &str) -> String {
        let base = format!("https://example.test/{tag}");
        serde_json::json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "body": format!("### inkread {tag}\n- fixes"),
            "assets": [
                {"name": format!("inkread-{tag}.apk"), "browser_download_url": format!("{base}/inkread-{tag}.apk")},
                {"name": format!("inkread-{tag}.apk.sha256"), "browser_download_url": format!("{base}/inkread-{tag}.apk.sha256")},
            ],
        })
        .to_string()
    }

    /// Parse the JSON `decide` returns back into a typed view for assertions.
    fn decision_of(installed: &str, json: &str) -> Decision {
        let s = decide(installed, json);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        Decision {
            update_available: v["updateAvailable"].as_bool().unwrap(),
            version: v["version"].as_str().unwrap_or_default().to_string(),
            notes: v["notes"].as_str().unwrap_or_default().to_string(),
            apk_url: v["apkUrl"].as_str().unwrap_or_default().to_string(),
            sha256_url: v["sha256Url"].as_str().unwrap_or_default().to_string(),
        }
    }

    #[test]
    fn newer_tag_is_offered_with_asset_urls() {
        let d = decision_of("0.3.0", &release_json("v0.4.0"));
        assert!(d.update_available);
        assert_eq!(d.version, "0.4.0");
        assert_eq!(d.apk_url, "https://example.test/v0.4.0/inkread-v0.4.0.apk");
        assert_eq!(
            d.sha256_url,
            "https://example.test/v0.4.0/inkread-v0.4.0.apk.sha256"
        );
        assert!(d.notes.contains("fixes"));
    }

    #[test]
    fn equal_version_is_not_offered() {
        assert!(!decision_of("0.4.0", &release_json("v0.4.0")).update_available);
    }

    #[test]
    fn older_tag_is_not_offered() {
        assert!(!decision_of("0.5.0", &release_json("v0.4.0")).update_available);
    }

    #[test]
    fn v_prefix_is_tolerated_on_both_sides() {
        // Installed "v0.3.0" (defensive) vs tag "0.4.0" (no v) still compares correctly.
        assert!(decision_of("v0.3.0", &release_json("0.4.0")).update_available);
    }

    #[test]
    fn prerelease_installed_upgrades_to_stable() {
        // The M0 build ships VERSION_NAME "0.1.0-m0"; a "0.1.0" release supersedes it (semver: a
        // prerelease precedes its release).
        assert!(decision_of("0.1.0-m0", &release_json("v0.1.0")).update_available);
    }

    #[test]
    fn stable_is_not_downgraded_to_a_prerelease_of_same_version() {
        // 0.1.0 installed vs a 0.1.0-rc tag: the rc is *older* than the release → not offered.
        assert!(!decision_of("0.1.0", &release_json("v0.1.0-rc.1")).update_available);
    }

    #[test]
    fn missing_apk_asset_is_not_offered() {
        let json = r#"{"tag_name":"v9.0.0","draft":false,"assets":[
            {"name":"notes.txt","browser_download_url":"https://example.test/notes.txt"}]}"#;
        assert!(!decision_of("0.3.0", json).update_available);
    }

    #[test]
    fn missing_checksum_still_offers_with_empty_sha_url() {
        let json = r#"{"tag_name":"v9.0.0","draft":false,"assets":[
            {"name":"inkread-v9.0.0.apk","browser_download_url":"https://example.test/a.apk"}]}"#;
        let d = decision_of("0.3.0", json);
        assert!(d.update_available);
        assert!(d.sha256_url.is_empty());
    }

    #[test]
    fn draft_release_is_ignored() {
        let json = r#"{"tag_name":"v9.0.0","draft":true,"assets":[
            {"name":"inkread-v9.0.0.apk","browser_download_url":"https://example.test/a.apk"}]}"#;
        assert!(!decision_of("0.3.0", json).update_available);
    }

    #[test]
    fn malformed_json_is_benign() {
        assert_eq!(decide("0.3.0", "not json at all"), Decision::NONE_JSON);
        assert_eq!(decide("0.3.0", ""), Decision::NONE_JSON);
        assert_eq!(decide("0.3.0", "{}"), Decision::NONE_JSON);
    }

    #[test]
    fn unparseable_installed_version_declines() {
        assert!(!decision_of("not-a-version", &release_json("v9.0.0")).update_available);
    }

    #[test]
    fn unparseable_tag_declines() {
        let json = r#"{"tag_name":"latest","draft":false,"assets":[
            {"name":"inkread.apk","browser_download_url":"https://example.test/a.apk"}]}"#;
        assert!(!decision_of("0.3.0", json).update_available);
    }

    #[test]
    fn none_decision_serializes_without_empty_fields() {
        // The benign result is exactly the documented constant — no null/empty noise the shell
        // must special-case.
        assert_eq!(decide("0.3.0", "garbage"), "{\"updateAvailable\":false}");
    }
}
