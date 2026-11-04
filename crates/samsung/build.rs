//! Bundles ReTV Home for Samsung (`apps/retv-tizen`, release files only)
//! into the crate. `service/dist/` must be built first
//! (`node apps/retv-tizen/service/build.js`); without it the bundle is empty
//! and the app says this build cannot install ReTV Home.
//!
//! `RETV_TIZEN_DIR` points at another app folder.

use std::path::{Path, PathBuf};

#[path = "src/bundle_rules.rs"]
mod bundle_rules;

fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let rel = path
            .strip_prefix(base)
            .unwrap()
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        if path.is_dir() {
            if !rel.split('/').any(|p| p.starts_with('.')) {
                println!("cargo:rerun-if-changed={}", path.display());
                walk(base, &path, out);
            }
        } else if bundle_rules::shipped(&rel) {
            println!("cargo:rerun-if-changed={}", path.display());
            out.push((rel, path));
        }
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=RETV_TIZEN_DIR");
    println!("cargo:rerun-if-changed=src/bundle_rules.rs");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = std::env::var("RETV_TIZEN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // In the ReTV monorepo: apps/retv-tizen beside apps/retv-bridge
            // (this crate is apps/retv-bridge/crates/samsung).
            let apps = manifest.ancestors().nth(3).unwrap_or(&manifest);
            let mono = apps.join("retv-tizen");
            // In the public repo (scripts/publish-retv-bridge.sh): vendor/retv-tizen at its root.
            let vendor = manifest.ancestors().nth(2).unwrap_or(&manifest).join("vendor/retv-tizen");
            if mono.join("config.xml").is_file() { mono } else { vendor }
        });
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files = Vec::new();
    if dir.join("service/dist/index.js").is_file() && dir.join("config.xml").is_file() {
        walk(&dir, &dir, &mut files);
        files.sort();
    } else {
        println!(
            "cargo:warning=ReTV Home for Samsung is not bundled: run `node apps/retv-tizen/service/build.js` first ({})",
            dir.display()
        );
    }
    let mut code = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for (rel, path) in &files {
        let abs = path.canonicalize().unwrap();
        code.push_str(&format!(
            "    ({rel:?}, include_bytes!({:?})),\n",
            abs.display().to_string()
        ));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("wgt.rs");
    std::fs::write(out, code).unwrap();
}
