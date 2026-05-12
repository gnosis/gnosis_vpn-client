fn main() {
    // Same linker group as gnosis_vpn-root: nftnl and mnl have circular
    // dependencies when built as static libs from nix.
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rustc-link-arg=-Wl,--start-group");
        println!("cargo:rustc-link-lib=static=nftnl");
        println!("cargo:rustc-link-lib=static=mnl");
        println!("cargo:rustc-link-arg=-Wl,--end-group");
    }
}
