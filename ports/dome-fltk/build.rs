use std::{env, path::PathBuf, process::Command};

fn output(program: &str, args: &[&str]) -> String {
    let result = Command::new(program).args(args).output().expect(program);
    assert!(
        result.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}
fn main() {
    println!("cargo:rerun-if-changed=native");
    println!("cargo:rerun-if-env-changed=DOME_FLTK_PREFIX");
    let prefix = PathBuf::from(
        env::var_os("DOME_FLTK_PREFIX").unwrap_or_else(|| "../../../_toolkit".into()),
    );
    let prefix = prefix
        .canonicalize()
        .expect("Set DOME_FLTK_PREFIX to the patched FLTK 1.4.5 prefix");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    // Servo's final link is large; use Rust's bundled lld without a system install.
    let sysroot = output(
        &env::var("RUSTC").unwrap_or_else(|_| "rustc".into()),
        &["--print", "sysroot"],
    );
    let bundled_lld = PathBuf::from(sysroot.trim())
        .join("lib/rustlib")
        .join(env::var("HOST").unwrap())
        .join("bin/rust-lld");
    if bundled_lld.is_file() {
        let linker = out.join("ld.lld");
        let _ = std::fs::remove_file(&linker);
        std::os::unix::fs::symlink(bundled_lld, &linker).unwrap();
        println!("cargo:rustc-link-arg=-B{}", out.display());
        println!("cargo:rustc-link-arg=-fuse-ld=lld");
    }
    for (mode, name) in [
        ("client-header", "appmenu-client.h"),
        ("private-code", "appmenu-protocol.c"),
    ] {
        assert!(
            Command::new("wayland-scanner")
                .args([mode, "native/appmenu.xml"])
                .arg(out.join(name))
                .status()
                .unwrap()
                .success()
        );
    }
    let flags = output(
        "pkg-config",
        &[
            "--cflags",
            "gio-2.0",
            "gio-unix-2.0",
            "wayland-client",
            "x11",
            "fontconfig",
            "cairo",
        ],
    );
    let mut cpp = cc::Build::new();
    if env::var_os("CARGO_FEATURE_OUTLINE_FONTS").is_some() {
        cpp.define("DOME_OUTLINE_FONTS", "1");
    } else {
        let runtime = PathBuf::from(env::var_os("DOME_BDF_RUNTIME_PREFIX")
            .unwrap_or_else(|| "/opt/fltk-vr/bdf-runtime".into()));
        assert!(runtime.join("share/domed-bdf-runtime/manifest.json").is_file(),
            "Build the DomeD native BDF HarfBuzz runtime first");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", runtime.join("lib").display());
    }
    println!("cargo:rerun-if-env-changed=DOME_BDF_RUNTIME_PREFIX");
    cpp.cpp(true)
        .std("c++17")
        .file("native/browser.cpp")
        .file("native/desktop-menu.cpp")
        .include(prefix.join("include"))
        .include(&out)
        .flag("-Wno-deprecated-declarations");
    for flag in flags.split_whitespace() {
        cpp.flag(flag);
    }
    cpp.compile("dome_fltk_ui");
    // GNU ld resolves archives from left to right. The UI references protocol
    // symbols, so emit their archive after the UI (lld also accepts this order).
    let mut c = cc::Build::new();
    c.file(out.join("appmenu-protocol.c"))
        .include(&out)
        .compile("dome_appmenu_protocol");
    println!(
        "cargo:rustc-link-search=native={}",
        prefix.join("lib").display()
    );
    for lib in [
        "fltk_images",
        "fltk",
        "fontconfig",
        "gio-2.0",
        "gobject-2.0",
        "glib-2.0",
        "wayland-client",
        "X11",
    ] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    println!(
        "cargo:rustc-link-arg=-Wl,-rpath,{}",
        prefix.join("lib").display()
    );
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../lib/dome-browser");
}
