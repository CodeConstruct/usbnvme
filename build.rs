fn main() {
    println!("cargo:rustc-link-arg-bins=--nmagic");
    // println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tlink-ram.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
}
