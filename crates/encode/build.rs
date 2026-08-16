use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-link-lib=vpx");

    if let Ok(lib_dir) = env::var("VPX_LIB_DIR") {
        println!("cargo:rustc-link-search=native={}", lib_dir);
    } else if cfg!(target_os = "macos") {
        let dir = "/opt/homebrew/lib";
        if std::path::Path::new(dir).exists() {
            println!("cargo:rustc-link-search=native={}", dir);
        }
    }

    // Generate bindings from system headers so struct layouts match the
    // installed libvpx version, avoiding the ABI mismatch with libvpx-sys
    // 1.4.2 (which was generated against an older libvpx).
    let mut builder = bindgen::Builder::default();

    if let Ok(include_dir) = env::var("VPX_INCLUDE_DIR") {
        builder = builder.clang_arg(format!("-I{}", include_dir));
    } else if cfg!(target_os = "macos") {
        let dir = "/opt/homebrew/include";
        if std::path::Path::new(dir).exists() {
            builder = builder.clang_arg(format!("-I{}", dir));
        }
    }

    let bindings = builder
        .header_contents(
            "vpx_wrapper.h",
            r#"
            #include <vpx/vpx_encoder.h>
            #include <vpx/vpx_decoder.h>
            #include <vpx/vpx_codec.h>
            #include <vpx/vpx_image.h>
            #include <vpx/vp8cx.h>
            "#,
        )
        .allowlist_function("vpx_.*")
        .allowlist_type("vpx_.*")
        .allowlist_type("vp8e_.*")
        .allowlist_type("vp9e_.*")
        .allowlist_var("VPX_.*")
        .allowlist_var("VP8.*")
        .allowlist_var("VP9.*")
        .allowlist_var("vp8e_.*")
        .allowlist_var("vp9e_.*")
        .derive_default(true)
        .derive_debug(true)
        .generate()
        .expect("Unable to generate vpx bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("vpx_bindings.rs"))
        .expect("Couldn't write vpx bindings");
}
