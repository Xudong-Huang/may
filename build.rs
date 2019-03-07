#[cfg(target_os = "linux")]
extern crate bindgen;
extern crate rustc_version;

use rustc_version::{version_meta, Channel};

#[cfg(target_os = "linux")]
fn gen_linux_aio_bindings() {
    use std::env;
    use std::path::PathBuf;
    // The bindgen::Builder is the main entry point
    // to bindgen, and lets you build up options for
    // the resulting bindings.
    let bindings = bindgen::Builder::default()
        .trust_clang_mangling(false)
        // The input header we would like to generate
        // bindings for.
        .header("linux_aio_wrapper.h")
        // Finish the builder and generate the bindings.
        .generate()
        // Unwrap the Result and panic on failure.
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/linux_bindings.rs file.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("linux_bindings.rs"))
        .expect("Couldn't write linux_bindings.rs!");
}

fn main() {
    // Set cfg flags depending on release channel
    if let Channel::Nightly = version_meta().unwrap().channel {
        println!("cargo:rustc-cfg=nightly");
    }

    #[cfg(target_os = "linux")]
    gen_linux_aio_bindings();
}
