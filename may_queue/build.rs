#[rustversion::nightly]
const NIGHTLY: bool = true;

#[rustversion::not(nightly)]
const NIGHTLY: bool = false;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(nightly)");
    // Set cfg flags depending on release channel
    if NIGHTLY {
        println!("cargo:rustc-cfg=nightly");
    }
}
