use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn watch_git_path(root: &Path, path: Option<String>) {
    if let Some(path) = path {
        let path = PathBuf::from(path);
        let path = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn emit_code_identity(root: &Path) {
    watch_git_path(root, git(root, &["rev-parse", "--git-path", "HEAD"]));
    let head_ref = git(root, &["symbolic-ref", "--quiet", "HEAD"]);
    watch_git_path(
        root,
        head_ref
            .as_deref()
            .and_then(|name| git(root, &["rev-parse", "--git-path", name])),
    );
    let commit = git(root, &["rev-parse", "HEAD"]).filter(|value| {
        value.len() == 40 && value.chars().all(|character| character.is_ascii_hexdigit())
    });
    let identity = match commit {
        Some(commit) => {
            if git(root, &["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|status| !status.trim().is_empty())
            {
                format!("{commit}-dirty")
            } else {
                commit
            }
        }
        None => "unknown".to_string(),
    };
    println!("cargo:rustc-env=QX_GIT_COMMIT={identity}");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Cargo did not set CARGO_MANIFEST_DIR for qx-app build script",
        )
    })?);
    let workspace_root = manifest.join("../..");
    emit_code_identity(&workspace_root);
    Ok(())
}
