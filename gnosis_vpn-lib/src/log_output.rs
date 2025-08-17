use humantime::format_duration;
use serde::ser::Serialize;

use std::time::SystemTime;

use crate::address::Address;
use crate::session;

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

pub fn print_node_access_instructions() {
    tracing::error!(
        r#"

>>!!>> Unable to access hoprd node API.
>>!!>> It seems you provided an invalid access token.
>>!!>> Please update your API token in the configuration file:
>>!!>> [hoprd_node]
>>!!>> api_token = "<your API token>"
"#
    );
}

pub fn print_node_port_instructions() {
    tracing::error!(
        r#"

>>!!>> Unable to connect to hoprd node API due to invalid endpoint port.
>>!!>> Please update your endpoint with the correct API port in the configuration file:
>>!!>> [hoprd_node]
>>!!>> endpoint = "<your hoprd node endpoint>"
"#
    );
}

pub fn print_node_timeout_instructions() {
    tracing::error!(
        r#"

>>!!>> Unable to connect to hoprd node API due to invalid IP address or offline status.
>>!!>> Please ensure you are connected to the internet and that your hoprd node is online.
>>!!>> In case of an invalid IP address please update your endpoint with the correct API IP in the configuration file:
>>!!>> [hoprd_node]
>>!!>> endpoint = "<your hoprd node endpoint>"
"#
    );
}

pub fn print_port_instructions(port: u16, protocol: session::Protocol) {
    let prot_str = match protocol {
        session::Protocol::Udp => "UDP",
        session::Protocol::Tcp => "TCP",
    };
    tracing::error!(
        r#"

>>!!>> It seems your node isn’t exposing the configured internal_connection_port ({}) on {}.
>>!!>> Please expose that port for both TCP and UDP.
>>!!>> Additionally add port mappings in your docker-compose.yml or to your docker run statement.
>>!!>> Alternatively, update your configuration file to use a different port.
"#,
        port,
        prot_str,
    );
}

pub fn print_no_destinations() {
    tracing::error!(
        r#"

>>!!>> No destinations found in configuration file.
>>!!>> Please rerun installer from https://raw.githubusercontent.com/gnosis/gnosis_vpn-client/heads/main/install.sh.
"#
    );
}

pub fn print_session_path_instructions() {
    tracing::error!(
        r#"

>>!!>> Cannot transport data through session.
>>!!>> This could mean you are missing channel connections to relayers.
>>!!>> Please check your hoprd node and open channels to relayers as specified here: https://github.com/gnosis/gnosis_vpn-client/blob/main/ONBOARDING.md#relayers.
"#
    );
}

pub fn print_invalid_session_config_parameter() {
    tracing::error!(
        r#"

>>!!>> Either your buffer size or max surb upstream parameter configuration is invalid.
>>!!>> Buffer sizes must be human readable bytesize values like "1.5 MB" or "10 KB".
>>!!>> Detailed docs at https://docs.rs/bytesize/2.0.1/bytesize/.
>>!!>>
>>!!>> Max surb upstream values need to be a human readable bandwidth value like "1 mbps" or "10 kbps".
>>!!>> Detailed docs at https://docs.rs/human-bandwidth/0.1.4/human_bandwidth/.
>>!!>>
>>!!>> Please update your configuration file:
>>!!>>
>>!!>> [connection.buffer]
>>!!>> bridge = "0 B"
>>!!>> ping = "0 B"
>>!!>> main = "1.5 MB"
>>!!>>
>>!!>> [connection.max_surb_upstream]
>>!!>> bridge = "0 bps"
>>!!>> ping = "0 bps"
>>!!>> main = "1.0 mbps"
"#
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
