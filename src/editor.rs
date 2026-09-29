//! Editor integration. Port of `src/utils/editor.ts`.
//!
//! The editor is the one place the CLI deliberately hands the terminal to a
//! child: descriptions are multi-line and need real keystrokes. It runs through
//! [`proc::run_inherit`] with stdin inherited (no `stdin_input`), so the child
//! owns the TTY while still being bounded by [`proc::EDITOR_TIMEOUT`].
//!
//! Every failure path returns `None` and prints an explanation to stderr,
//! matching upstream's `console.error` + `return undefined`.

use std::path::{Path, PathBuf};

use crate::proc;

/// The editor to use: `git config --global core.editor` when set (and `git` is
/// available), else `$EDITOR`, else `None`.
pub fn get_editor() -> Option<String> {
    if let Some(output) = proc::run(
        "git",
        &["config", "--global", "core.editor"],
        &proc::RunOptions::default(),
        proc::DEFAULT_TIMEOUT,
    ) {
        if output.success {
            let editor = output.stdout_trimmed();
            if !editor.is_empty() {
                return Some(editor);
            }
        }
    }

    match std::env::var("EDITOR") {
        Ok(editor) if !editor.is_empty() => Some(editor),
        _ => None,
    }
}

/// Open `$EDITOR` on a fresh temporary markdown file and return its trimmed
/// contents, or `None` when no editor is configured, the editor fails, or the
/// buffer comes back empty.
pub fn open_editor() -> Option<String> {
    let Some(editor) = get_editor() else {
        eprintln!(
            "No editor found. Please set EDITOR environment variable or configure git editor with: git config --global core.editor <editor>"
        );
        return None;
    };

    let Some(temp_file) = create_temp_markdown() else {
        eprintln!("Failed to open editor: could not create a temporary file");
        return None;
    };

    let result = run_editor(&editor, &temp_file);
    // Upstream removes the temp file in a `finally`; do the same whether the
    // editor succeeded, failed, or the read failed.
    let _ = std::fs::remove_file(&temp_file);
    result
}

fn run_editor(editor: &str, path: &Path) -> Option<String> {
    let path_arg = path.to_string_lossy().to_string();
    match proc::run_inherit(editor, &[&path_arg], None, proc::EDITOR_TIMEOUT) {
        Some(true) => {}
        Some(false) => {
            eprintln!("Editor exited with an error");
            return None;
        }
        None => {
            eprintln!("Failed to open editor: the editor could not be started");
            return None;
        }
    }

    match std::fs::read_to_string(path) {
        Ok(content) => {
            let cleaned = content.trim();
            if cleaned.is_empty() {
                None
            } else {
                Some(cleaned.to_string())
            }
        }
        Err(error) => {
            eprintln!("Failed to open editor: {error}");
            None
        }
    }
}

/// Create an empty `*.md` file in the system temp directory.
fn create_temp_markdown() -> Option<PathBuf> {
    let dir = std::env::temp_dir();
    for attempt in 0..100 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let name = format!("linear-{}-{nanos}-{attempt}.md", std::process::id());
        let path = dir.join(name);
        if !path.exists() {
            std::fs::File::create(&path).ok()?;
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_temp_markdown_makes_a_unique_existing_file() {
        let first = create_temp_markdown().expect("temp file");
        let second = create_temp_markdown().expect("temp file");
        assert!(first.exists());
        assert!(second.exists());
        assert_ne!(first, second);
        assert_eq!(first.extension().and_then(|ext| ext.to_str()), Some("md"));
        let _ = std::fs::remove_file(&first);
        let _ = std::fs::remove_file(&second);
    }
}
