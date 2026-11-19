fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    if std::env::var("TARGET").as_deref() == Ok("x86_64-unknown-none") {
        println!("cargo:rustc-link-arg-bins=-T{dir}/../cell.ld");
        println!("cargo:rustc-link-arg-bins=--no-dynamic-linker");
    }
    println!("cargo:rerun-if-changed={dir}/../cell.ld");
}
