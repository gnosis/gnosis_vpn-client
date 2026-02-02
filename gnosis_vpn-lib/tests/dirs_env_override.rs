#[cfg(test)]
mod tests {
    use gnosis_vpn_lib::dirs;
    use std::env;
    use std::fs;

    #[test]
    fn test_override() {
        let tmp = env::temp_dir().join("gnosis_override_test");
        let _ = fs::remove_dir_all(&tmp);

        unsafe {
            env::set_var("GNOSISVPN_HOME", &tmp);
        }

        let res = dirs::config_dir("test.toml");
        assert!(res.is_ok());
        let path = res.unwrap();

        println!("Path: {}", path.display());
        assert_eq!(path, tmp.join("test.toml"));

        let res_cache = dirs::cache_dir("test.cache");
        assert!(res_cache.is_ok());
        let path_cache = res_cache.unwrap();
        println!("Cache Path: {}", path_cache.display());
        assert_eq!(path_cache, tmp.join("cache").join("test.cache"));

        unsafe {
            env::remove_var("GNOSISVPN_HOME");
        }
    }
}
