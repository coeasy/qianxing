#![deny(clippy::expect_used, clippy::unwrap_used)]

use sha2::{Digest, Sha256};
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

fn watch(root: &Path, path: Option<String>) {
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Cargo did not provide CARGO_MANIFEST_DIR to qx-python build script",
        )
    })?;
    let manifest = PathBuf::from(manifest_dir);
    let root = manifest.join("../..");
    watch(&root, git(&root, &["rev-parse", "--git-path", "HEAD"]));
    let head_ref = git(&root, &["symbolic-ref", "--quiet", "HEAD"]);
    watch(
        &root,
        head_ref
            .as_deref()
            .and_then(|name| git(&root, &["rev-parse", "--git-path", name])),
    );

    let commit = git(&root, &["rev-parse", "HEAD"]).filter(|value| {
        value.len() == 40 && value.chars().all(|character| character.is_ascii_hexdigit())
    });
    let identity = match commit {
        Some(commit) => {
            if git(&root, &["status", "--porcelain", "--untracked-files=no"])
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

    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=QX_TARGET_TRIPLE={target}");
    println!("cargo:rustc-env=QX_BUILD_PROFILE={profile}");

    let schema_dir = root.join("schemas");
    println!("cargo:rerun-if-changed={}", schema_dir.display());
    let mut schemas = Vec::new();
    for entry in std::fs::read_dir(&schema_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            schemas.push(path);
        }
    }
    schemas.sort();
    let mut digest = Sha256::new();
    for path in schemas {
        println!("cargo:rerun-if-changed={}", path.display());
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("schema filename is not valid UTF-8: {}", path.display()),
                )
            })?;
        let body = std::fs::read(&path)?;
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name.as_bytes());
        digest.update((body.len() as u64).to_be_bytes());
        digest.update(body);
    }
    println!(
        "cargo:rustc-env=QX_SCHEMA_CATALOG_SHA256={:x}",
        digest.finalize()
    );
    Ok(())
}
