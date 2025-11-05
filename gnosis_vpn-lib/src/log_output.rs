use edgli::hopr_lib::Address;
use humantime::format_duration;
use serde::ser::Serialize;

use std::time::SystemTime;

use crate::hopr::config as hopr_config;

pub fn serialize<T>(v: &T) -> String
where
    T: ?Sized + Serialize,
{
    match serde_json::to_string(&v) {
        Ok(s) => s,
        Err(e) => format!("serialization error: {e}"),
    }
}

pub fn elapsed(timestamp: &SystemTime) -> String {
    match timestamp.elapsed() {
        Ok(elapsed) => truncate_after_second_space(format_duration(elapsed).to_string().as_str()).to_string(),
        Err(e) => format!("error displaying duration: {e}"),
    }
}

pub fn address(address: &Address) -> String {
    let str = address.to_string();
    format!("{}..{}", &str[..6], &str[38..])
}

fn truncate_after_second_space(s: &str) -> &str {
    let spaces = s.match_indices(' ').take(2);
    if let Some((index, _)) = spaces.last() {
        &s[..index]
    } else {
        s
    }
}

pub fn print_safe_module_storage_error(main_error: hopr_config::Error) {
    let file = match hopr_config::safe_file() {
        Ok(path) => path,
        Err(error) => {
            tracing::error!(
                r#"

>>!!>> Critical error storing safe module after safe deployment:
>>!!>>
>>!!>> {main_error:?}.
>>!!>>
>>!!>> Cannot determine safe file path: {error:?}.
"#,
            );
            return;
        }
    };
    let parent = match file.parent() {
        Some(p) => p,
        None => {
            tracing::error!(
                r#"

>>!!>> Critical error storing safe module after safe deployment:
>>!!>>
>>!!>> {main_error:?}.
>>!!>>
>>!!>> Cannot determine safe file parent folder path.
"#,
            );
            return;
        }
    };
    tracing::error!(
        r#"

>>!!>> Critical error storing safe module after safe deployment:
>>!!>>
>>!!>> {main_error:?}.
>>!!>>
>>!!>> If this is a permission problem, please fix permissions on folder "{parent}".
>>!!>> So that writing the safe file "{file}" will work.
>>!!>> Otherwise check for out of disk space issues or other IO related problems.
"#,
        parent = parent.display(),
        file = file.display()
    );
}

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
