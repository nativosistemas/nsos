fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // Solo enlazamos con nuestro script cuando compilamos para bare metal.
    if std::env::var("TARGET").as_deref() == Ok("x86_64-unknown-none") {
        println!("cargo:rustc-link-arg-bins=-T{dir}/linker-x86_64.ld");
        println!("cargo:rustc-link-arg-bins=--no-dynamic-linker");
    }
    println!("cargo:rerun-if-changed={dir}/linker-x86_64.ld");
}
