fn main() {
    // Ensure mnl is linked after nftnl to resolve undefined references
    // when building with static libraries from nix
    #[cfg(target_os = "linux")]
    {
        // Use linker group to handle circular dependencies between static libs
        println!("cargo:rustc-link-arg=-Wl,--start-group");
        println!("cargo:rustc-link-lib=static=nftnl");
        println!("cargo:rustc-link-lib=static=mnl");
        println!("cargo:rustc-link-arg=-Wl,--end-group");
    }
}
