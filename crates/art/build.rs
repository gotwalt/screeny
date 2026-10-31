//! On macOS, embed `Info.plist` in the `screeny-art` binary, as
//! `crates/screeny/build.rs` does for `screeny`: `screeny-art play` talks to
//! panels on the LAN, and macOS looks here for the Local Network usage string
//! and the Bonjour service type. Sign it with `tools/sign-macos.sh screeny-art`.
fn main() {
    println!("cargo:rerun-if-changed=Info.plist");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("Info.plist");
        println!("cargo:rustc-link-arg-bin=screeny-art=-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
    }
}
