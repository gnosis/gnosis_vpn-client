pub fn print_session_established(path: &str) {
    tracing::info!(
        r#"

            /---==========================---\
            |   VPN CONNECTION ESTABLISHED   |
            \---==========================---/

            route: {}
        "#,
        path
    );
}
