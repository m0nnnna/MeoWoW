//! Puts the client's icon on the executable.
//!
//! Windows reads an application's icon out of the binary's resource table, not
//! from anything the program does at runtime -- so the icon in Explorer, on
//! the taskbar and in Alt-Tab is decided here and the one on the window itself
//! is decided in `icon.rs`. Both draw the same cat: `src/icon_art.rs` is
//! `include!`d rather than imported, because a build script cannot depend on
//! the crate it is building.
//!
//! **A missing resource compiler is a warning, not a failed build.** `rc.exe`
//! comes with the Windows SDK and is normally there beside the MSVC toolchain,
//! but it is not part of Rust, and a client that will not compile because a
//! decoration could not be attached would be the wrong trade by a wide margin.

include!("src/icon_art.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/icon_art.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let icon = out.join("meowow.ico");
    // The four sizes Windows actually asks for: a title bar, a taskbar, a
    // large-icon folder view, and the 256 that everything modern scales from.
    if let Err(e) = std::fs::write(&icon, ico(&[16, 32, 48, 256])) {
        println!("cargo:warning=could not write {}: {e}", icon.display());
        return;
    }

    let script = out.join("meowow.rc");
    // Resource id 1: Windows shows the *lowest-numbered* icon resource as the
    // application's, so this is not an arbitrary number.
    let text = format!("1 ICON \"{}\"\n", icon.display().to_string().replace('\\', "/"));
    if let Err(e) = std::fs::write(&script, text) {
        println!("cargo:warning=could not write {}: {e}", script.display());
        return;
    }

    let result = embed_resource::compile(&script, embed_resource::NONE);
    if let Err(e) = result.manifest_optional() {
        println!("cargo:warning=the executable has no icon: {e}");
    }
}
