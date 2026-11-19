fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // Solo enlazamos con nuestro script cuando compilamos para bare metal.
    let script = match std::env::var("TARGET").as_deref() {
        Ok("x86_64-unknown-none") => Some("linker-x86_64.ld"),
        Ok("aarch64-unknown-none-softfloat") => Some("linker-aarch64.ld"),
        _ => None,
    };
    if let Some(script) = script {
        println!("cargo:rustc-link-arg-bins=-T{dir}/{script}");
        println!("cargo:rustc-link-arg-bins=--no-dynamic-linker");
        println!("cargo:rustc-link-arg-bins=-zmax-page-size=4096");
    }
    println!("cargo:rerun-if-changed={dir}/linker-x86_64.ld");
    println!("cargo:rerun-if-changed={dir}/linker-aarch64.ld");
}
