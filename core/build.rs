//! Works out the version at build time (the same sum as scripts/version.sh): the major.minor in the VERSION file,
//! then the number of commits since that file last changed. `MC_VERSION` overrides it, for builds from a copy of
//! the files that has no git history. Without either, the patch number is 0.

use std::path::Path;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (output.status.success() && !text.is_empty()).then_some(text)
}

fn main() {
    println!("cargo:rerun-if-env-changed=MC_VERSION");
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it");
    let root = Path::new(&manifest).parent().expect("the crate is in the project's folder");
    // Only what exists: a path that isn't there would make cargo run this again on every build.
    for watched in ["VERSION", ".git/HEAD", ".git/logs/HEAD"] {
        if root.join(watched).exists() {
            println!("cargo:rerun-if-changed={}", root.join(watched).display());
        }
    }
    let given = std::env::var("MC_VERSION").ok().filter(|version| !version.trim().is_empty());
    let version = given.unwrap_or_else(|| {
        let base = std::fs::read_to_string(root.join("VERSION")).expect("the VERSION file at the project's root");
        let patch = git(root, &["log", "-1", "--format=%H", "--", "VERSION"]).and_then(|since| git(root, &["rev-list", "--count", &format!("{since}..HEAD")])).unwrap_or_else(|| "0".into());
        format!("{}.{patch}", base.trim())
    });
    println!("cargo:rustc-env=MC_VERSION={}", version.trim());
}
