use std::{
    env, fs,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(root)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", root.display()))
        .map(|entry| entry.expect("failed to read source entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_files(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut files = Vec::new();
    for relative in ["src", "migrations", "templates"] {
        println!("cargo::rerun-if-changed={relative}");
        collect_files(&manifest_dir.join(relative), &mut files);
    }
    for relative in ["Cargo.toml", "Cargo.lock", "build.rs"] {
        files.push(manifest_dir.join(relative));
    }
    files.sort();

    let mut hasher = Sha256::new();
    let target = env::var("TARGET").expect("TARGET is not set by Cargo");
    hasher.update(b"target\0");
    hasher.update(target.as_bytes());
    let mut features = env::vars()
        .filter_map(|(key, _)| key.strip_prefix("CARGO_FEATURE_").map(str::to_owned))
        .collect::<Vec<_>>();
    features.sort();
    for feature in features {
        hasher.update(b"feature\0");
        hasher.update(feature.as_bytes());
        hasher.update([0]);
    }
    for path in files {
        let relative = path.strip_prefix(&manifest_dir).unwrap();
        println!("cargo::rerun-if-changed={}", relative.display());
        let bytes = fs::read(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }

    println!(
        "cargo::rustc-env=RBPH_BUILD_FINGERPRINT={:x}",
        hasher.finalize()
    );
}
