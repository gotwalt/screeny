//! On macOS, embed `Info.plist` in the `screeny` binary's `__TEXT,__info_plist`
//! section. A bare executable has no bundle, and this is where the system looks for
//! `NSLocalNetworkUsageDescription` / `NSBonjourServices` when deciding whether (and
//! with what wording) to ask for Local Network access. See card 015 and
//! `tools/sign-macos.sh`.
fn main() {
    println!("cargo:rerun-if-changed=Info.plist");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("Info.plist");
        println!(
            "cargo:rustc-link-arg-bin=screeny=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}
