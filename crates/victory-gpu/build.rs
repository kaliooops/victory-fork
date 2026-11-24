//! The translator's fingerprint for the shader cache (`src/shader_cache.rs`):
//! a BLAKE3 of everything that decides what a console shader translates to,
//! that is the translator crate's sources and manifest, the cache entry's
//! encoding, and the locked dependency versions. A rebuild that changes none
//! of them (any change elsewhere, or a diagnostics build as against a release
//! one) keeps the cache; one that changes any of them starts a fresh one.

use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let root = manifest.join("../..");
    let inputs = [
        root.join("crates/victory-shader/src"),
        root.join("crates/victory-shader/Cargo.toml"),
        manifest.join("src/shader_cache.rs"),
        root.join("Cargo.lock"),
        root.join("rust-toolchain.toml"),
    ];

    let mut files = Vec::new();
    for input in &inputs {
        println!("cargo:rerun-if-changed={}", input.display());
        collect(input, &mut files);
    }
    files.sort();

    let mut hasher = blake3::Hasher::new();
    for file in &files {
        // The path relative to the workspace, so that the fingerprint is the
        // same wherever the checkout is, then the length and the bytes.
        let name = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(file).unwrap_or_else(|e| panic!("reading {}: {e}", file.display()));
        hasher.update(name.as_bytes());
        hasher.update(&[0]);
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    println!("cargo:rustc-env=VICTORY_TRANSLATOR_HASH={}", hasher.finalize().to_hex());
}

/// `path` itself if it is a file, every file under it if it is a folder.
fn collect(path: &Path, files: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).unwrap_or_else(|e| panic!("listing {}: {e}", path.display())) {
            collect(&entry.expect("a folder entry").path(), files);
        }
    } else if path.is_file() {
        files.push(path.to_path_buf());
    }
}
