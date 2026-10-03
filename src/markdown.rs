//! Linear-flavored Markdown guidance and Linear-upload image caching. Port of
//! `src/utils/markdown-help.ts` and `src/utils/markdown-images.ts`.
//!
//! The help strings live here so the many Markdown-writing commands and the
//! `linear markdown` reference cannot drift apart. The image helpers download
//! images and Linear-upload links referenced from a Markdown body into a cache
//! directory and rewrite the body to point at the local copies, so a terminal
//! can display them.
//!
//! Divergence from upstream: the Deno version round-trips the body through
//! remark, which also normalizes unrelated Markdown. This port extracts the
//! inline image/link URLs with a scanner and substitutes those URLs in place,
//! leaving the rest of the body byte-for-byte unchanged. `[text][ref]`-style
//! reference links are not resolved.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};

use crate::consts;
use crate::errors::{CliError, Result};
use crate::graphql;

/// Appended as a second paragraph to the description of every command that
/// takes a rich Markdown body. It carries the rule an agent gets wrong when it
/// has never been told (`@name` mentions nobody) plus the lookup it needs next.
pub const MARKDOWN_HINT: &str =
    "Linear Markdown: a plain Linear URL creates a mention; `@name`, `@[Name](id)`,\n\
and `[Name](url)` do not. Get a person's URL from the `url` field of\n\
`linear team members <TEAM> --json`, or an issue's from `linear issue url <ID>`.\n\
Run `linear markdown` for collapsible sections and the full reference.";

/// Joins a command's own summary line to the shared Markdown hint.
pub fn with_markdown_hint(description: &str) -> String {
    format!("{description}\n\n{MARKDOWN_HINT}")
}

/// Used both as the `markdown` command's description and as what it prints.
pub const LINEAR_MARKDOWN_REFERENCE: &str =
    "Linear-flavored Markdown: mentions and collapsible sections\n\
\n\
These rules apply to comment bodies, issue descriptions, document content,\n\
project overviews, and status update bodies.\n\
\n\
MENTIONS\n\
\n\
A resource's plain Linear URL becomes a linked mention. A literal `@name`, an\n\
`@[Name](id)`, or a Markdown link such as `[Name](url)` does not — it stays\n\
plain text and notifies nobody. Put the bare URL in the body:\n\
\n\
https://linear.app/acme/profiles/someuser can you take a look?\n\
\n\
RESOLVING PEOPLE\n\
\n\
Look the person up in the relevant team first. The team can usually be\n\
inferred from the issue identifier or the current directory:\n\
\n\
linear team members ENG --json\n\
\n\
Paste the selected member's `url` field verbatim. If the intended person is\n\
not a member of that team, stop and confirm before searching the whole\n\
workspace with `linear user list --json`; mentioning someone outside the team\n\
is likely accidental.\n\
\n\
To mention an issue, use its URL the same way:\n\
\n\
linear issue url ENG-123\n\
\n\
COLLAPSIBLE SECTIONS\n\
\n\
Open a section with `+++ [title]` and close it with `+++`:\n\
\n\
+++ [Server log]\n\
\n\
Markdown content that is initially hidden.\n\
\n\
+++\n\
\n\
The square brackets around the title and the closing `+++` are both required.";

// ---------------------------------------------------------------------------
// Upload-aware image caching
// ---------------------------------------------------------------------------

/// The host Linear serves authenticated uploads from.
fn private_upload_host() -> &'static str {
    consts::LINEAR_PRIVATE_UPLOAD_HOST
}

/// The upload host for a URL, when it is a Linear upload host.
pub fn get_linear_upload_host(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    consts::LINEAR_UPLOAD_HOSTNAMES
        .iter()
        .find(|candidate| **candidate == host)
        .map(|candidate| (*candidate).to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub url: String,
    pub alt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    pub url: String,
    pub text: Option<String>,
}

/// `![alt](url)` / `[text](url)`, tolerating an optional `"title"`.
fn inline_link_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(!?)\[([^\]]*)\]\(([^\s)]+)(?:\s+"[^"]*")?\)"#)
            .expect("valid inline link regex")
    })
}

/// Images referenced from a Markdown body, in document order.
pub fn extract_image_info(content: Option<&str>) -> Vec<ImageInfo> {
    let Some(content) = content else {
        return Vec::new();
    };
    let mut images = Vec::new();
    for caps in inline_link_regex().captures_iter(content) {
        if &caps[1] != "!" {
            continue;
        }
        let url = caps[3].to_string();
        if url.is_empty() {
            continue;
        }
        let alt = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        images.push(ImageInfo {
            url,
            alt: if alt.is_empty() {
                None
            } else {
                Some(alt.to_string())
            },
        });
    }
    images
}

/// Links in a Markdown body that point at a Linear upload host.
pub fn extract_linear_link_info(content: Option<&str>) -> Vec<LinkInfo> {
    let Some(content) = content else {
        return Vec::new();
    };
    let mut links = Vec::new();
    for caps in inline_link_regex().captures_iter(content) {
        if &caps[1] == "!" {
            continue;
        }
        let url = caps[3].to_string();
        if url.is_empty() || get_linear_upload_host(&url).is_none() {
            continue;
        }
        let text = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        links.push(LinkInfo {
            url,
            text: if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            },
        });
    }
    links
}

/// The cache directory for downloaded images.
pub fn image_cache_dir() -> PathBuf {
    let base = std::env::var("TMPDIR")
        .or_else(|_| std::env::var("TMP"))
        .or_else(|_| std::env::var("TEMP"))
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/tmp".to_string());
    Path::new(&base).join("linear-cli-images")
}

/// The first 16 hex characters of the URL's SHA-256 digest.
fn url_hash(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    hex[..16].to_string()
}

/// Keep a filename safe on any filesystem: no separators, no control chars,
/// no leading dots, and never empty.
pub(crate) fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').to_string();
    if trimmed.is_empty() {
        "image".to_string()
    } else {
        trimmed
    }
}

fn download_image(url: &str, alt_text: Option<&str>) -> Result<PathBuf> {
    let image_dir = image_cache_dir().join(url_hash(url));
    std::fs::create_dir_all(&image_dir)
        .map_err(|error| CliError::cli(format!("Failed to create image cache dir: {error}")))?;

    let filename = match alt_text {
        Some(alt) if !alt.is_empty() => sanitize_filename(alt),
        _ => "image".to_string(),
    };
    let filepath = image_dir.join(&filename);
    if filepath.exists() {
        return Ok(filepath);
    }

    download_linear_file(url, &filepath, "image")?;
    Ok(filepath)
}

/// Download `url` to `destination`, adding the API-key header for private
/// Linear uploads. The parent directory must already exist. `label` names the
/// artifact in error messages ("image", "attachment").
pub(crate) fn download_linear_file(url: &str, destination: &Path, label: &str) -> Result<()> {
    let mut request = upload_agent().get(url);
    if get_linear_upload_host(url).as_deref() == Some(private_upload_host()) {
        if let Ok(api_key) = graphql::resolve_api_key() {
            request = request.header("Authorization", &api_key);
        }
    }

    let mut response = request
        .call()
        .map_err(|error| CliError::cli(format!("Failed to download {label}: {error}")))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let status_text = response.status().canonical_reason().unwrap_or("");
        return Err(CliError::cli(format!(
            "Failed to download {label}: {status} {status_text}"
        )));
    }

    let mut bytes = Vec::new();
    std::io::copy(&mut response.body_mut().as_reader(), &mut bytes)
        .map_err(|error| CliError::cli(format!("Failed to read {label} body: {error}")))?;
    // Staged and renamed rather than written in place: a reader - including the next run, which
    // treats an existing file as a cache hit - must never see a prefix of this download.
    crate::atomic::write(destination, &bytes)
        .map_err(|error| CliError::cli(format!("Failed to write {label}: {error}")))?;
    Ok(())
}

fn upload_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(crate::net::request_timeout()))
        .timeout_connect(Some(crate::net::connect_timeout()))
        .build()
        .new_agent()
}

/// Download every image and Linear-upload link referenced from one or more
/// Markdown sources, returning a map of original URL to local file path. A
/// single download failure is reported on stderr but does not abort the rest.
pub fn download_markdown_images(sources: &[Option<&str>]) -> HashMap<String, String> {
    let mut files_by_url: Vec<(String, Option<String>)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for source in sources {
        for image in extract_image_info(*source) {
            if seen.insert(image.url.clone()) {
                files_by_url.push((image.url, image.alt));
            }
        }
        for link in extract_linear_link_info(*source) {
            if seen.insert(link.url.clone()) {
                files_by_url.push((link.url, link.text));
            }
        }
    }

    let mut url_to_path = HashMap::new();
    for (url, alt) in files_by_url {
        match download_image(&url, alt.as_deref()) {
            Ok(path) => {
                url_to_path.insert(url, path.to_string_lossy().into_owned());
            }
            Err(error) => {
                eprintln!("Failed to download {url}: {}", error.user_message);
            }
        }
    }
    url_to_path
}

/// Rewrite every known URL in `content` to its local path. Longer URLs are
/// substituted first so a URL that is a prefix of another cannot corrupt it.
pub fn replace_urls(content: &str, url_to_path: &HashMap<String, String>) -> String {
    let mut entries: Vec<(&String, &String)> = url_to_path.iter().collect();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.0.len()));
    let mut result = content.to_string();
    for (url, path) in entries {
        result = result.replace(url.as_str(), path.as_str());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_hint_is_appended_after_a_blank_line() {
        assert!(with_markdown_hint("Do a thing").starts_with("Do a thing\n\nLinear Markdown:"));
    }

    #[test]
    fn extracts_images_with_and_without_alt() {
        let images = extract_image_info(Some("![logo](https://x/y.png) and ![](https://x/z.png)"));
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].alt.as_deref(), Some("logo"));
        assert_eq!(images[1].alt, None);
    }

    #[test]
    fn only_linear_upload_links_are_extracted() {
        let content = "[a](https://uploads.linear.app/1) [b](https://example.com/2)";
        let links = extract_linear_link_info(Some(content));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url, "https://uploads.linear.app/1");
    }

    #[test]
    fn upload_host_detection_ignores_lookalikes() {
        assert_eq!(
            get_linear_upload_host("https://uploads.linear.app/a").as_deref(),
            Some("uploads.linear.app")
        );
        assert_eq!(get_linear_upload_host("https://evil.test/a"), None);
        assert_eq!(get_linear_upload_host("not a url"), None);
    }

    #[test]
    fn url_hash_is_stable_and_short() {
        let hash = url_hash("https://uploads.linear.app/a");
        assert_eq!(hash.len(), 16);
        assert_eq!(hash, url_hash("https://uploads.linear.app/a"));
        assert!(hash.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn sanitize_filename_strips_separators_and_empty_names() {
        assert_eq!(sanitize_filename("a/b:c"), "a_b_c");
        assert_eq!(sanitize_filename("   "), "image");
        assert_eq!(sanitize_filename("..."), "image");
    }

    #[test]
    fn replace_urls_prefers_longer_urls() {
        let mut map = HashMap::new();
        map.insert("https://x/a".to_string(), "/short".to_string());
        map.insert("https://x/a/b".to_string(), "/long".to_string());
        let out = replace_urls("![i](https://x/a/b) and https://x/a", &map);
        assert_eq!(out, "![i](/long) and /short");
    }
}
