use humantime::format_duration;
use serde::ser::Serialize;

use std::time::SystemTime;

use crate::session;

pub fn serialize<T>(v: &T) -> String
where
    T: ?Sized + Serialize,
{
    match serde_json::to_string(&v) {
        Ok(s) => s,
        Err(e) => format!("serializion error: {}", e),
    }
}

pub fn elapsed(timestamp: &SystemTime) -> String {
    match timestamp.elapsed() {
        Ok(elapsed) => truncate_after_second_space(format_duration(elapsed).to_string().as_str()).to_string(),
        Err(e) => format!("error displaying duration: {}", e),
    }
}

pub fn peer_id(id: &str) -> String {
    let l = id.len();
    format!(".{}", &id[l - 4..])
}

fn truncate_after_second_space(s: &str) -> &str {
    let spaces = s.match_indices(' ').take(2);
    if let Some((index, _)) = spaces.last() {
        &s[..index]
    } else {
        s
    }
}

pub fn print_port_instructions(port: u16, protocol: session::Protocol) {
    tracing::error!(
        r#"

>>!!>> It seems your node isn’t exposing the configured internal_connection_port ({}) on {}.
>>!!>> Please expose that port for both TCP and UDP.
>>!!>> Additionally add port mappings in your docker-compose.yml or to your docker run statement.
>>!!>> Alternatively, update your configuration file to use a different port.
"#,
        port,
        protocol
    );
}

pub fn print_wg_manual_instructions() {
    tracing::error!(
        r#"

>>!!>> If you intend to use manual WireGuard mode, please add your public key to the configuration file:
>>!!>> [wireguard]
>>!!>> manual_mode = {{ public_key = "<wg public key>" }}
"#
    );
}
