use anyhow::Result;
use gnosis_vpn_lib::config;
use gnosis_vpn_lib::config::v1::{SessionCapabilitiesConfig, SessionPathConfig, SessionTargetConfig};
use reqwest::blocking;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::cmp;
use std::fmt;
use std::thread;
use url::Url;

use crate::entry_node::EntryNode;
use crate::event::Event;
use crate::remote_data;

#[derive(Debug, Serialize, Deserialize)]
pub struct Session {
    // listen host
    pub ip: String,
    pub port: u16,
    pub protocol: String,
    pub target: String,
}

#[derive(Debug)]
pub struct OpenSession {
    pub endpoint: Url,
    pub api_token: String,
    pub destination: String,
    pub capabilities: Option<Vec<SessionCapabilitiesConfig>>,
    pub listen_host: Option<String>,
    pub path: Option<SessionPathConfig>,
    pub target: Option<SessionTargetConfig>,
}

#[tracing::instrument(skip(client, sender), level = tracing::Level::DEBUG)]
pub fn open(
    client: &blocking::Client,
    sender: &crossbeam_channel::Sender<Event>,
    open_session: &OpenSession,
) -> Result<()> {
    let headers = remote_data::authentication_headers(open_session.api_token.as_str())?;
    let url = open_session.endpoint.join("api/v3/session/udp")?;
    let mut json = serde_json::Map::new();
    json.insert("destination".to_string(), json!(open_session.destination));

    let target = open_session.target.clone().unwrap_or_default();
    let target_type = target.type_.unwrap_or_default();
    let target_host = target.host.unwrap_or(config::v1::default_session_target_host());
    let target_port = target.port.unwrap_or(config::v1::default_session_target_port());

    let target_json = json!({ target_type.to_string(): format!("{}:{}", target_host, target_port) });
    json.insert("target".to_string(), target_json);
    let path_json = match open_session.path.clone() {
        Some(SessionPathConfig::Hop(hop)) => {
            json!({"Hops": hop})
        }
        Some(SessionPathConfig::Intermediates(ids)) => {
            json!({ "IntermediatePath": ids.clone() })
        }
        None => {
            json!({"Hops": 1})
        }
    };
    json.insert("path".to_string(), path_json);
    if let Some(lh) = &open_session.listen_host {
        json.insert("listenHost".to_string(), json!(lh));
    };

    let capabilities_json = match &open_session.capabilities {
        Some(caps) => {
            json!(caps)
        }
        None => {
            json!(["Segmentation"])
        }
    };
    json.insert("capabilities".to_string(), capabilities_json);

    let sender = sender.clone();
    let client = client.clone();
    thread::spawn(move || {
        tracing::debug!(?headers, body = ?json, ?url, "post open session");

        let fetch_res = client
            .post(url)
            .json(&json)
            .timeout(std::time::Duration::from_secs(30))
            .headers(headers)
            .send()
            .map(|res| (res.status(), res.json::<serde_json::Value>()));

        let evt = match fetch_res {
            Ok((status, Ok(json))) if status.is_success() => {
                Event::FetchOpenSession(remote_data::Event::Response(json))
            }
            Ok((status, Ok(json))) => {
                let e = remote_data::CustomError {
                    reqw_err: None,
                    status: Some(status),
                    value: Some(json),
                };
                Event::FetchOpenSession(remote_data::Event::Error(e))
            }
            Ok((status, Err(e))) => {
                let e = remote_data::CustomError {
                    reqw_err: Some(e),
                    status: Some(status),
                    value: None,
                };
                Event::FetchOpenSession(remote_data::Event::Error(e))
            }
            Err(e) => {
                let e = remote_data::CustomError {
                    reqw_err: Some(e),
                    status: None,
                    value: None,
                };
                Event::FetchOpenSession(remote_data::Event::Error(e))
            }
        };

        sender.send(evt)
    });
    Ok(())
}

pub fn schedule_check_session(
    delay: std::time::Duration,
    sender: &crossbeam_channel::Sender<Event>,
) -> crossbeam_channel::Sender<()> {
    let sender = sender.clone();
    let (cancel_sender, cancel_receiver) = crossbeam_channel::bounded(1);
    thread::spawn(move || {
        crossbeam_channel::select! {
            recv(cancel_receiver) -> _ => {}
            default(delay) => {
                let res = sender.send(Event::CheckSession);
            match res {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("sending delayed event failed: {}", e);
                }
            }
            }
        }
    });
    cancel_sender
}

pub fn schedule_retry_open(
    delay: std::time::Duration,
    sender: &crossbeam_channel::Sender<Event>,
) -> crossbeam_channel::Sender<()> {
    let sender = sender.clone();
    let (cancel_sender, cancel_receiver) = crossbeam_channel::bounded(1);
    thread::spawn(move || {
        crossbeam_channel::select! {
            recv(cancel_receiver) -> _ => {}
            default(delay) => {
            let res = sender.send(Event::FetchOpenSession(remote_data::Event::Retry));
            match res {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("sending delayed event failed: {}", e);
                }
            }
            }
        }
    });
    cancel_sender
}

pub fn schedule_retry_close(
    delay: std::time::Duration,
    sender: &crossbeam_channel::Sender<Event>,
) -> crossbeam_channel::Sender<()> {
    let sender = sender.clone();
    let (cancel_sender, cancel_receiver) = crossbeam_channel::bounded(1);
    thread::spawn(move || {
        crossbeam_channel::select! {
            recv(cancel_receiver) -> _ => {}
            default(delay) => {
            let res = sender.send(Event::FetchCloseSession(remote_data::Event::Retry));
            match res {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("sending delayed event failed: {}", e);
                }
            }
            }
        }
    });
    cancel_sender
}

impl Session {
    pub fn verify_open(&self, sessions: &[Session]) -> bool {
        sessions.iter().any(|entry| entry == self)
    }

    pub fn close(
        &self,
        client: &blocking::Client,
        sender: &crossbeam_channel::Sender<Event>,
        entry_node: &EntryNode,
    ) -> Result<()> {
        let headers = remote_data::authentication_headers(entry_node.api_token.as_str())?;
        let path = format!("api/v3/session/udp/{}/{}", self.ip, self.port);
        let url = entry_node.endpoint.join(path.as_str())?;

        let sender = sender.clone();
        let client = client.clone();

        thread::spawn(move || {
            tracing::debug!(?headers, ?url, "delete session");

            let fetch_res = client
                .delete(url)
                .timeout(std::time::Duration::from_secs(30))
                .headers(headers)
                .send()
                .map(|res| (res.status(), res.json::<serde_json::Value>()));

            let evt = match fetch_res {
                Ok((status, Ok(json))) if status.is_success() => {
                    Event::FetchCloseSession(remote_data::Event::Response(json))
                }
                Ok((status, Ok(json))) => {
                    let e = remote_data::CustomError {
                        reqw_err: None,
                        status: Some(status),
                        value: Some(json),
                    };
                    Event::FetchCloseSession(remote_data::Event::Error(e))
                }
                // TODO hanlde empty expected response better
                Ok((status, Err(_))) if status.is_success() => {
                    Event::FetchCloseSession(remote_data::Event::Response(serde_json::Value::Null))
                }
                Ok((status, Err(e))) => {
                    let e = remote_data::CustomError {
                        reqw_err: Some(e),
                        status: Some(status),
                        value: None,
                    };
                    Event::FetchCloseSession(remote_data::Event::Error(e))
                }
                Err(e) => {
                    let e = remote_data::CustomError {
                        reqw_err: Some(e),
                        status: None,
                        value: None,
                    };
                    Event::FetchCloseSession(remote_data::Event::Error(e))
                }
            };

            sender.send(evt)
        });
        Ok(())
    }
}

impl cmp::PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        self.ip == other.ip && self.port == other.port && self.protocol == other.protocol && self.target == other.target
    }
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Session[{}:{} {} {}]",
            self.ip, self.port, self.protocol, self.target
        )
    }
}
