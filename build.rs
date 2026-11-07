// Embeds the exe icon: lld and link.exe both take a .res file as a linker input.
// The file is copied into OUT_DIR first because the linker driver mangles non-ASCII paths.
fn main() {
    println!("cargo:rerun-if-changed=assets/vitals.res");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let res = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("vitals.res");
        std::fs::copy("assets/vitals.res", &res).expect("assets/vitals.res exists");
        println!("cargo:rustc-link-arg-bins={}", res.display());
    }
}
