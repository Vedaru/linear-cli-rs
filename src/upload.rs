//! File upload to Linear's cloud storage. Port of `src/utils/upload.ts`.
//!
//! Uploading is two steps: ask the GraphQL API for a signed URL, then PUT the
//! bytes to that URL. Uploads default to private (workspace-members only),
//! matching the Linear web app; public is opt-in and only valid for raster
//! images, where requesting it for anything else is an error rather than a
//! silent downgrade.

use std::path::Path;

use serde_json::{json, Value};

use crate::errors::{CliError, Result};
use crate::graphql;

/// Maximum file size for uploads (100MB).
pub const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;

const FILE_UPLOAD_MUTATION: &str = r#"
    mutation FileUpload($contentType: String!, $filename: String!, $size: Int!, $makePublic: Boolean) {
      fileUpload(contentType: $contentType, filename: $filename, size: $size, makePublic: $makePublic) {
        success
        uploadFile {
          assetUrl
          uploadUrl
          headers {
            key
            value
          }
        }
      }
    }
"#;

/// MIME type for a lowercased file extension. `None` for unknown extensions.
fn mime_type_for_extension(extension: &str) -> Option<&'static str> {
    let mime = match extension {
        // Images
        ".png" => "image/png",
        ".jpg" | ".jpeg" => "image/jpeg",
        ".gif" => "image/gif",
        ".webp" => "image/webp",
        ".svg" => "image/svg+xml",
        ".ico" => "image/x-icon",
        ".bmp" => "image/bmp",
        ".tiff" | ".tif" => "image/tiff",
        // Documents
        ".pdf" => "application/pdf",
        ".doc" => "application/msword",
        ".docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ".xls" => "application/vnd.ms-excel",
        ".xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ".ppt" => "application/vnd.ms-powerpoint",
        ".pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        // Text
        ".txt" => "text/plain",
        ".md" | ".markdown" => "text/markdown",
        ".csv" => "text/csv",
        ".tsv" => "text/tab-separated-values",
        ".html" | ".htm" => "text/html",
        ".css" => "text/css",
        ".xml" => "text/xml",
        // Code
        ".js" | ".mjs" | ".jsx" => "text/javascript",
        ".ts" | ".tsx" => "text/typescript",
        ".json" => "application/json",
        ".yaml" | ".yml" => "text/yaml",
        ".toml" => "text/toml",
        ".sh" | ".bash" => "text/x-shellscript",
        ".py" => "text/x-python",
        ".rb" => "text/x-ruby",
        ".go" => "text/x-go",
        ".rs" => "text/x-rust",
        ".java" => "text/x-java",
        ".c" | ".h" => "text/x-c",
        ".cpp" | ".hpp" => "text/x-c++",
        // Archives
        ".zip" => "application/zip",
        ".tar" => "application/x-tar",
        ".gz" => "application/gzip",
        ".7z" => "application/x-7z-compressed",
        ".rar" => "application/vnd.rar",
        // Audio
        ".mp3" => "audio/mpeg",
        ".wav" => "audio/wav",
        ".ogg" => "audio/ogg",
        ".m4a" => "audio/mp4",
        // Video
        ".mp4" => "video/mp4",
        ".webm" => "video/webm",
        ".mov" => "video/quicktime",
        ".avi" => "video/x-msvideo",
        // Other
        ".wasm" => "application/wasm",
        _ => return None,
    };
    Some(mime)
}

/// MIME type from a file path's extension, defaulting to `application/octet-stream`.
pub fn get_mime_type(filepath: &str) -> String {
    let extension = Path::new(filepath)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{}", extension.to_lowercase()))
        .unwrap_or_default();
    mime_type_for_extension(&extension)
        .unwrap_or("application/octet-stream")
        .to_string()
}

/// Result of a successful file upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadResult {
    /// The permanent URL where the file is accessible.
    pub asset_url: String,
    /// The original filename.
    pub filename: String,
    /// The file size in bytes.
    pub size: u64,
    /// The MIME type of the file.
    pub content_type: String,
    /// Whether the file was uploaded to a public, unauthenticated URL.
    pub public: bool,
}

/// Options for file upload.
#[derive(Debug, Clone, Default)]
pub struct UploadOptions {
    /// Upload the file to a public, unauthenticated URL. Only supported for
    /// raster images. Defaults to private to match the Linear web app.
    pub make_public: Option<bool>,
}

/// Linear only allows public uploads for raster images (excluding SVG).
fn can_be_public(content_type: &str) -> bool {
    matches!(
        content_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp" | "image/tiff"
    )
}

/// Resolve the effective `make_public` value for an upload.
///
/// Public is opt-in and only valid for raster image types — requesting it for
/// any other content type is an error rather than a silent downgrade.
pub fn resolve_make_public(content_type: &str, requested: Option<bool>) -> Result<bool> {
    let make_public = requested.unwrap_or(false);
    if make_public && !can_be_public(content_type) {
        let subject = if content_type.is_empty() {
            "this file"
        } else {
            content_type
        };
        return Err(CliError::validation(format!(
            "Cannot upload {subject} to a public URL"
        ))
        .suggestion(
            "Linear only allows public uploads for raster images (png, jpeg, gif, webp, bmp, tiff). \
             Remove --public to upload privately.",
        ));
    }
    Ok(make_public)
}

fn filename_of(filepath: &str) -> String {
    Path::new(filepath)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(filepath)
        .to_string()
}

/// Upload a file to Linear's cloud storage, returning the asset URL and file
/// metadata.
pub fn upload_file(filepath: &str, options: &UploadOptions) -> Result<UploadResult> {
    let metadata = std::fs::metadata(filepath).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CliError::not_found("File", filepath)
        } else {
            CliError::validation(format!("Not a file: {filepath}"))
                .suggestion("Please provide a path to a valid file")
        }
    })?;
    if !metadata.is_file() {
        return Err(CliError::validation(format!("Not a file: {filepath}"))
            .suggestion("Please provide a path to a valid file"));
    }

    let size = metadata.len();
    if size > MAX_FILE_SIZE {
        return Err(CliError::validation(format!(
            "File too large: {:.2}MB exceeds limit of {}MB",
            size as f64 / 1024.0 / 1024.0,
            MAX_FILE_SIZE / 1024 / 1024
        ))
        .suggestion("Please upload a file smaller than 100MB"));
    }

    let filename = filename_of(filepath);
    let content_type = get_mime_type(filepath);

    // Default to private; public is an explicit opt-in and only valid for
    // images. Validate before doing any network work.
    let make_public = resolve_make_public(&content_type, options.make_public)?;

    let client = graphql::client()?;
    let data = client.request(
        FILE_UPLOAD_MUTATION,
        json!({
            "contentType": content_type,
            "filename": filename,
            "size": size,
            "makePublic": make_public,
        }),
    )?;

    let file_upload = data.get("fileUpload").cloned().unwrap_or(Value::Null);
    let success = file_upload
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let upload_file = file_upload
        .get("uploadFile")
        .filter(|value| !value.is_null());
    let (Some(upload_file), true) = (upload_file, success) else {
        return Err(CliError::cli("Failed to get upload URL from Linear"));
    };

    let asset_url = upload_file
        .get("assetUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::cli("Linear returned an upload without an asset URL"))?
        .to_string();
    let upload_url = upload_file
        .get("uploadUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| CliError::cli("Linear returned an upload without an upload URL"))?
        .to_string();

    let file_data = std::fs::read(filepath)
        .map_err(|error| CliError::cli(format!("Failed to read {filepath}: {error}")))?;

    // Content-Type is required by the signed URL; Linear's returned headers may
    // override it.
    let mut request = upload_agent()
        .put(&upload_url)
        .header("content-type", &content_type);
    if let Some(headers) = upload_file.get("headers").and_then(Value::as_array) {
        for header in headers {
            let key = header.get("key").and_then(Value::as_str);
            let value = header.get("value").and_then(Value::as_str);
            if let (Some(key), Some(value)) = (key, value) {
                request = request.header(key, value);
            }
        }
    }

    let mut response = request
        .send(file_data.as_slice())
        .map_err(|error| CliError::cli(format!("Failed to upload file: {error}")))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        let status_text = response.status().canonical_reason().unwrap_or("");
        return Err(CliError::cli(format!(
            "Failed to upload file: {status} {status_text} - {}",
            body.trim()
        )));
    }

    Ok(UploadResult {
        asset_url,
        filename,
        size,
        content_type,
        public: make_public,
    })
}

fn upload_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(crate::net::request_timeout()))
        .timeout_connect(Some(crate::net::connect_timeout()))
        .build()
        .new_agent()
}

/// Upload several files in order.
pub fn upload_files(filepaths: &[String], options: &UploadOptions) -> Result<Vec<UploadResult>> {
    let mut results = Vec::with_capacity(filepaths.len());
    for filepath in filepaths {
        results.push(upload_file(filepath, options)?);
    }
    Ok(results)
}

/// Check that a path exists and is a readable file.
pub fn validate_file_path(filepath: &str) -> Result<()> {
    match std::fs::metadata(filepath) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(CliError::validation(format!("Not a file: {filepath}"))
            .suggestion("Please provide a path to a valid file")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(CliError::not_found("File", filepath))
        }
        Err(error) => Err(CliError::cli(format!("Cannot read {filepath}: {error}"))),
    }
}

/// Format an uploaded file as a Markdown link (image syntax for images).
pub fn format_as_markdown_link(result: &UploadResult) -> String {
    if result.content_type.starts_with("image/") {
        format!("![{}]({})", result.filename, result.asset_url)
    } else {
        format!("[{}]({})", result.filename, result.asset_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_types_cover_known_and_unknown_extensions() {
        assert_eq!(get_mime_type("photo.PNG"), "image/png");
        assert_eq!(get_mime_type("/a/b/notes.md"), "text/markdown");
        assert_eq!(get_mime_type("archive.bin"), "application/octet-stream");
        assert_eq!(get_mime_type("noextension"), "application/octet-stream");
    }

    #[test]
    fn public_uploads_are_restricted_to_raster_images() {
        assert!(resolve_make_public("image/png", Some(true)).unwrap());
        assert!(!resolve_make_public("image/png", None).unwrap());
        let error = resolve_make_public("image/svg+xml", Some(true)).unwrap_err();
        assert_eq!(error.kind, crate::errors::ErrorKind::Validation);
    }

    #[test]
    fn image_uploads_use_image_markdown() {
        let result = UploadResult {
            asset_url: "https://uploads.linear.app/a.png".into(),
            filename: "a.png".into(),
            size: 1,
            content_type: "image/png".into(),
            public: false,
        };
        assert_eq!(
            format_as_markdown_link(&result),
            "![a.png](https://uploads.linear.app/a.png)"
        );
    }

    #[test]
    fn validate_file_path_reports_missing_files() {
        let error = validate_file_path("/definitely/not/here.txt").unwrap_err();
        assert_eq!(error.kind, crate::errors::ErrorKind::NotFound);
    }
}
