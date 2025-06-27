extern crate pkg_config;

use std::env;
use std::path::PathBuf;

#[cfg(feature = "mnl-1-0-4")]
const MIN_VERSION: &str = "1.0.4";
#[cfg(not(feature = "mnl-1-0-4"))]
const MIN_VERSION: &str = "1.0.3";


#[cfg(target_os = "linux")]
fn main() {
    println!("cargo:rerun-if-env-changed=LIBMNL_LIB_DIR");
    if let Some(lib_dir) = env::var_os("LIBMNL_LIB_DIR").map(PathBuf::from) {
        if !lib_dir.is_dir() {
            panic!(
                "libmnl library directory does not exist: {}",
                lib_dir.display()
            );
        }
        println!("cargo:rustc-link-search=native={}", lib_dir.display());
        println!("cargo:rustc-link-lib=mnl");
    } else {
        // Trying with pkg-config instead
        println!("Minimum libmnl version: {}", MIN_VERSION);
        pkg_config::Config::new()
            .atleast_version(MIN_VERSION)
            .probe("libmnl")
            .unwrap();
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This crate does nothing on non-Linux");
}
