use std::process::Command;

fn main() {
    println!("cargo:rustc-link-arg-bins=--nmagic");
    // println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tlink-ram.x");

    git();
}

fn git() {
    let rev = Command::new("git")
        .args([
            "-c",
            "core.abbrev=8",
            "rev-parse",
            "--verify",
            "--short",
            "HEAD",
        ])
        .output()
        .unwrap();
    let rev = String::from_utf8(rev.stdout).unwrap();
    let rev = rev.trim();
    assert_eq!(rev.len(), 8);

    // Determine local directory changes
    let modified = Command::new("git")
        .args(["ls-files", "--modified"])
        .output()
        .unwrap();
    let modified = String::from_utf8(modified.stdout).unwrap();
    let modified = modified.trim();
    let dirty = if modified.is_empty() { "" } else { "-dirty" };

    println!("cargo::rustc-env=GIT_REV={rev}{dirty}");

    // Find git directory
    let path_res = Command::new("git")
        .args(["rev-parse", "--path-format=relative", "--git-dir"])
        .output()
        .map(|o| String::from_utf8(o.stdout).unwrap().trim().to_string());
    if let Ok(path) = path_res {
        println!("cargo:rerun-if-changed={path}/HEAD");
    }

    // Workaround for
    // https://github.com/rust-lang/cargo/issues/4587
    // since setting any rerun-if-changed clears the default set.
    println!("cargo::rerun-if-changed=src");
    println!("cargo::rerun-if-changed=Cargo.lock");
    println!("cargo::rerun-if-changed=Cargo.toml");
}
