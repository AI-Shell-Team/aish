use std::borrow::Cow;
use std::io::Read;
use std::path::PathBuf;

use base64::Engine;

const IMAGE_EXTENSIONS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp",
];

const MAX_IMAGE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB per image
const MAX_TOTAL_ENCODED: usize = 50 * 1024 * 1024; // 50 MB total encoded budget

/// Info about a successfully attached image.
#[derive(Debug, Clone)]
pub struct AttachedImage {
    pub filename: String,
    pub size_bytes: u64,
}

pub struct ExtractedImage {
    pub cleaned_text: String,
    pub image_urls: Vec<String>,
    /// Successfully attached images with metadata.
    pub attached: Vec<AttachedImage>,
    /// User-visible warnings (shown in the terminal).
    pub warnings: Vec<String>,
}

/// Scan text for image file paths, encode them, and return cleaned text.
///
/// Only tokens that look like file paths (starting with `/`, `./`, `../`,
/// `~`, `$`) and resolve to existing image files are extracted.
/// Non-image paths and non-existent files are left in the text untouched.
pub fn extract_images(text: &str) -> ExtractedImage {
    // Fast path: if text contains no image file extensions, skip processing
    let has_image_ext = IMAGE_EXTENSIONS.iter().any(|ext| text.contains(ext));
    if !has_image_ext {
        return ExtractedImage {
            cleaned_text: text.to_string(),
            image_urls: Vec::new(),
            attached: Vec::new(),
            warnings: Vec::new(),
        };
    }

    let mut cleaned_parts: Vec<String> = Vec::new();
    let mut image_urls: Vec<String> = Vec::new();
    let mut attached: Vec<AttachedImage> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut total_encoded_bytes: usize = 0;

    // First pass: split on whitespace (handles quotes/escapes)
    let raw_tokens = split_preserving_escapes(text);
    // Second pass: split tokens that have an embedded absolute path after non-path prefix
    // e.g. "分析/tmp/img.png" → ["分析", "/tmp/img.png"]
    let tokens: Vec<String> = raw_tokens.iter().flat_map(|t| split_embedded_path(t)).collect();
    for token in &tokens {
        if let Some(result) = try_resolve_image(token) {
            match result {
                ImageResolveResult::Encoded { url, filename, size_bytes } => {
                    let encoded_len = url.len();
                    if total_encoded_bytes + encoded_len > MAX_TOTAL_ENCODED {
                        warnings.push(format!(
                            "📎 Total image size exceeds 50 MB budget, skipping: {}",
                            filename
                        ));
                        cleaned_parts.push(token.clone());
                        continue;
                    }
                    total_encoded_bytes += encoded_len;
                    image_urls.push(url);
                    attached.push(AttachedImage {
                        filename: filename.clone(),
                        size_bytes,
                    });
                    // Don't add replacement text — the image is sent as a separate
                    // content block and a text reference would mislead the model into
                    // trying to read the file with a tool.
                }
                ImageResolveResult::Warning(msg) => {
                    warnings.push(msg);
                    cleaned_parts.push(token.clone());
                }
            }
        } else {
            cleaned_parts.push(token.clone());
        }
    }

    ExtractedImage {
        cleaned_text: cleaned_parts.join(" "),
        image_urls,
        attached,
        warnings,
    }
}

enum ImageResolveResult {
    Encoded {
        url: String,
        filename: String,
        size_bytes: u64,
    },
    Warning(String),
}

/// Try to resolve a token as an image file path.
/// Returns None if the token is not a path candidate.
fn try_resolve_image(token: &str) -> Option<ImageResolveResult> {
    let cleaned = strip_quotes(token);
    if !looks_like_path(cleaned.as_ref()) {
        return None;
    }

    let path = match resolve_path(cleaned.as_ref()) {
        Some(p) => p,
        None => return None,
    };

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let ext_with_dot = format!(".{}", ext.to_lowercase());
    if !IMAGE_EXTENSIONS.contains(&ext_with_dot.as_str()) {
        return None;
    }

    // Open file once, get metadata from the handle to avoid TOCTOU
    let mut file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return None,
    };
    let metadata = match file.metadata() {
        Ok(m) => m,
        Err(_) => return None,
    };
    if !metadata.is_file() {
        return None;
    }

    if metadata.len() > MAX_IMAGE_SIZE {
        let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
        return Some(ImageResolveResult::Warning(format!(
            "📎 Image too large: {} ({:.1} MB, max 10 MB)",
            path.display(),
            size_mb
        )));
    }

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image")
        .to_string();
    let size_bytes = metadata.len();

    // Read from the already-open handle (no TOCTOU gap)
    let mut bytes = Vec::with_capacity(size_bytes as usize);
    match file.read_to_end(&mut bytes) {
        Ok(_) => {}
        Err(e) => {
            return Some(ImageResolveResult::Warning(format!(
                "📎 Failed to read image {}: {}",
                path.display(),
                e
            )));
        }
    }

    let url = encode_image_bytes(&bytes, &ext);
    Some(ImageResolveResult::Encoded {
        url,
        filename,
        size_bytes,
    })
}

fn looks_like_path(s: &str) -> bool {
    s.starts_with('/')
        || s.starts_with("./")
        || s.starts_with("../")
        || s.starts_with('~')
        || s.starts_with('$')
}

fn strip_quotes(s: &str) -> Cow<'_, str> {
    let len = s.len();
    if len >= 2
        && ((s.starts_with('"') && s.ends_with('"'))
            || (s.starts_with('\'') && s.ends_with('\'')))
    {
        Cow::Owned(s[1..len - 1].to_string())
    } else {
        Cow::Borrowed(s)
    }
}

/// Split text on whitespace, treating backslash-escaped spaces and
/// quoted strings as single tokens.
fn split_preserving_escapes(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes: Option<char> = None;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        match in_quotes {
            Some(q) => {
                if c == q {
                    in_quotes = None;
                }
                current.push(c);
            }
            None => {
                if c == '"' || c == '\'' {
                    in_quotes = Some(c);
                    current.push(c);
                } else if c == '\\' && chars.peek() == Some(&' ') {
                    current.push(' ');
                    chars.next();
                } else if c.is_whitespace() {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                } else {
                    current.push(c);
                }
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Split a token that has an embedded absolute path after a non-path prefix.
/// e.g. "分析/tmp/img.png" → vec!["分析", "/tmp/img.png"]
/// Returns a single-element vec if no embedded path is found.
fn split_embedded_path(token: &str) -> Vec<String> {
    // Already starts with / — no split needed
    // Quoted tokens are handled by strip_quotes later — don't split them here
    if token.starts_with('/') || token.starts_with('"') || token.starts_with('\'') || !token.contains('/') {
        return vec![token.to_string()];
    }

    // Check if this token contains a URL pattern (://) — if so, don't split
    if token.contains("://") {
        return vec![token.to_string()];
    }

    // Find the first '/' that starts a local absolute path suffix.
    let bytes = token.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i] != b'/' {
            continue;
        }
        // Skip double-slash (e.g. //host/path)
        if i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            continue;
        }
        let suffix = &token[i..];
        if suffix.ends_with('/') {
            continue;
        }
        // Must have a file extension
        if suffix.contains('.') {
            let prefix = token[..i].to_string();
            let path_part = suffix.to_string();
            return vec![prefix, path_part];
        }
    }

    vec![token.to_string()]
}

fn resolve_path(raw: &str) -> Option<PathBuf> {
    let expanded = shellexpand_home_and_env(raw);
    let path = PathBuf::from(&expanded);
    if path.is_absolute() {
        Some(path)
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

fn shellexpand_home_and_env(input: &str) -> String {
    shellexpand::full(input)
        .unwrap_or_else(|_| Cow::Borrowed(input))
        .into_owned()
}

fn encode_image_bytes(bytes: &[u8], ext: &str) -> String {
    let mime = match ext.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:{mime};base64,{b64}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_paths_in_text() {
        let result = extract_images("hello world");
        assert_eq!(result.cleaned_text, "hello world");
        assert!(result.image_urls.is_empty());
        assert!(result.attached.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_nonexistent_path_ignored() {
        let result = extract_images("look at /nonexistent/path/img.png");
        assert!(result.image_urls.is_empty());
        assert!(result.attached.is_empty());
        assert!(result.cleaned_text.contains("/nonexistent/path/img.png"));
    }

    #[test]
    fn test_non_image_extension_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello").unwrap();
        let result = extract_images(&format!("read {}", file_path.display()));
        assert!(result.image_urls.is_empty());
        assert!(result.attached.is_empty());
        assert!(result.cleaned_text.contains("test.txt"));
    }

    #[test]
    fn test_image_file_encoded() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.png");
        let png_bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        std::fs::write(&file_path, &png_bytes).unwrap();
        let result = extract_images(&format!("describe {}", file_path.display()));
        assert_eq!(result.image_urls.len(), 1);
        assert!(result.image_urls[0].starts_with("data:image/png;base64,"));
        // Image path is removed from text — the image is sent as a content block
        assert!(!result.cleaned_text.contains("test.png"));
        assert_eq!(result.attached.len(), 1);
        assert_eq!(result.attached[0].filename, "test.png");
    }

    #[test]
    fn test_multiple_images() {
        let dir = tempfile::tempdir().unwrap();
        let file1 = dir.path().join("a.png");
        let file2 = dir.path().join("b.jpg");
        std::fs::write(&file1, vec![0x89, 0x50, 0x4E, 0x47]).unwrap();
        std::fs::write(&file2, vec![0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
        let result = extract_images(&format!(
            "compare {} and {}",
            file1.display(),
            file2.display()
        ));
        assert_eq!(result.image_urls.len(), 2);
        assert_eq!(result.attached.len(), 2);
        assert!(result.image_urls[0].starts_with("data:image/png;base64,"));
        assert!(result.image_urls[1].starts_with("data:image/jpeg;base64,"));
        assert_eq!(result.attached[0].filename, "a.png");
        assert_eq!(result.attached[1].filename, "b.jpg");
    }

    #[test]
    fn test_quoted_path() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("my dir");
        std::fs::create_dir_all(&subdir).unwrap();
        let file_path = subdir.join("img.png");
        std::fs::write(&file_path, vec![0x89, 0x50, 0x4E, 0x47]).unwrap();
        let input = format!("describe \"{}\"", file_path.display());
        let result = extract_images(&input);
        assert_eq!(result.image_urls.len(), 1);
        assert_eq!(result.attached.len(), 1);
    }

    #[test]
    fn test_looks_like_path() {
        assert!(looks_like_path("/tmp/a.png"));
        assert!(looks_like_path("./a.png"));
        assert!(looks_like_path("../a.png"));
        assert!(looks_like_path("~/a.png"));
        assert!(looks_like_path("$HOME/a.png"));
        assert!(!looks_like_path("hello"));
        assert!(!looks_like_path("ls"));
    }

    #[test]
    fn test_split_preserving_escapes() {
        let tokens = split_preserving_escapes("a b c");
        assert_eq!(tokens, vec!["a", "b", "c"]);
        let tokens = split_preserving_escapes("a \"b c\" d");
        assert_eq!(tokens, vec!["a", "\"b c\"", "d"]);
        let tokens = split_preserving_escapes("a\\ b c");
        assert_eq!(tokens, vec!["a b", "c"]);
    }

    #[test]
    fn test_split_embedded_path() {
        // CJK text directly followed by absolute path
        let tokens = split_embedded_path("分析/tmp/img.png");
        assert_eq!(tokens, vec!["分析", "/tmp/img.png"]);

        // Already starts with / — no split
        let tokens = split_embedded_path("/tmp/img.png");
        assert_eq!(tokens, vec!["/tmp/img.png"]);

        // No slash — no split
        let tokens = split_embedded_path("hello");
        assert_eq!(tokens, vec!["hello"]);

        // URL should not be split (double slash)
        let tokens = split_embedded_path("看https://example.com/img.png");
        assert_eq!(tokens.len(), 1); // stays as one token
    }

    #[test]
    fn test_cjk_no_space_image_detection() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.png");
        std::fs::write(&file_path, vec![0x89, 0x50, 0x4E, 0x47]).unwrap();
        // CJK text directly adjacent to path — no space
        let input = format!("分析{}", file_path.display());
        let result = extract_images(&input);
        assert_eq!(result.image_urls.len(), 1);
        assert!(result.attached[0].filename == "test.png");
    }

    #[test]
    fn test_encode_image_png() {
        let url = encode_image_bytes(b"fake png data", "png");
        assert!(url.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn test_encode_image_jpeg() {
        let url = encode_image_bytes(b"fake jpeg", "jpg");
        assert!(url.starts_with("data:image/jpeg;base64,"));
    }
}
