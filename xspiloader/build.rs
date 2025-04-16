fn main() {
    println!("cargo:rustc-link-arg-bins=--nmagic");
    // path is relative to workspace root
    println!("cargo:rustc-link-arg-bins=-Txspiloader/link-bootloader.x");
}
