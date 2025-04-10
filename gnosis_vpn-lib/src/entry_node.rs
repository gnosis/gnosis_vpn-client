use anyhow::Result;
use gnosis_vpn_lib::log_output;
use gnosis_vpn_lib::peer_id::PeerId;
use reqwest::blocking;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::thread;
use url::Url;

use crate::event::Event;
use crate::remote_data;

#[derive(Debug)]
pub struct EntryNode {
    // TODO store multiple entry nodes and exit nodes and separate user_input
    pub endpoint: Url,
    pub api_token: String,
    pub listen_host: Option<String>,
    pub path: Path,
    pub addresses: Option<Addresses>,
}

#[derive(Debug)]
pub enum Path {
    Hop(u8),
    Intermediates(Vec<PeerId>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Addresses {
    hopr: String,
    native: String,
}

pub fn schedule_retry_query_addresses(
    delay: std::time::Duration,
    sender: &crossbeam_channel::Sender<Event>,
) -> crossbeam_channel::Sender<()> {
    let sender = sender.clone();
    let (cancel_sender, cancel_receiver) = crossbeam_channel::bounded(1);
    thread::spawn(move || {
        crossbeam_channel::select! {
            recv(cancel_receiver) -> _ => {}
            default(delay) => {
                match sender.send(Event::FetchAddresses(remote_data::Event::Retry)) {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "failed sending retry event");
                    }
                }
            }
        }
    });
    cancel_sender
}

pub fn schedule_retry_list_sessions(
    delay: std::time::Duration,
    sender: &crossbeam_channel::Sender<Event>,
) -> crossbeam_channel::Sender<()> {
    let sender = sender.clone();
    let (cancel_sender, cancel_receiver) = crossbeam_channel::bounded(1);
    thread::spawn(move || {
        crossbeam_channel::select! {
            recv(cancel_receiver) -> _ => {}
            default(delay) => {
            match sender.send(Event::FetchListSessions(remote_data::Event::Retry)) {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "failed sending retry event");
                }
            }
            }
        }
    });
    cancel_sender
}

impl fmt::Display for EntryNode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let dsp = match &self.addresses {
            Some(en_addresses) => log_output::peer_id(en_addresses.hopr.as_str()),
            None => self.endpoint.to_string(),
        };
        write!(f, "({})", dsp)
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let hops = match self {
            Path::Hop(hop) => "(hop)".repeat(*hop as usize),
            Path::Intermediates(ids) => ids
                .iter()
                .map(|id| format!("({})", log_output::peer_id(id.to_string().as_str())))
                .collect::<Vec<String>>()
                .join(""),
        };
        let dsp = hops.split(")(").collect::<Vec<&str>>().join(") <-> (");
        write!(f, "{}", dsp)
    }
}

impl EntryNode {
    pub fn new(endpoint: &Url, api_token: &str, listen_host: Option<&str>, path: Path) -> EntryNode {
        EntryNode {
            endpoint: endpoint.clone(),
            api_token: api_token.to_string(),
            addresses: None,
            listen_host: listen_host.map(|s| s.to_string()),
            path,
        }
    }

    pub fn query_addresses(&self, client: &blocking::Client, sender: &crossbeam_channel::Sender<Event>) -> Result<()> {
        let headers = remote_data::authentication_headers(self.api_token.as_str())?;
        let url = self.endpoint.join("api/v3/account/addresses")?;
        let sender = sender.clone();
        let client = client.clone();
        thread::spawn(move || {
            tracing::debug!(?headers, ?url, "get addresses");

            let fetch_res = client
                .get(url)
                .timeout(std::time::Duration::from_secs(30))
                .headers(headers)
                .send()
                .map(|res| (res.status(), res.json::<serde_json::Value>()));

            let evt = match fetch_res {
                Ok((status, Ok(json))) if status.is_success() => {
                    Event::FetchAddresses(remote_data::Event::Response(json))
                }
                Ok((status, Ok(json))) => {
                    let e = remote_data::CustomError {
                        reqw_err: None,
                        status: Some(status),
                        value: Some(json),
                    };
                    Event::FetchAddresses(remote_data::Event::Error(e))
                }
                Ok((status, Err(e))) => {
                    let e = remote_data::CustomError {
                        reqw_err: Some(e),
                        status: Some(status),
                        value: None,
                    };
                    Event::FetchAddresses(remote_data::Event::Error(e))
                }
                Err(e) => {
                    let e = remote_data::CustomError {
                        reqw_err: Some(e),
                        status: None,
                        value: None,
                    };
                    Event::FetchAddresses(remote_data::Event::Error(e))
                }
            };
            sender.send(evt)
        });
        Ok(())
    }

    pub fn list_sessions(&self, client: &blocking::Client, sender: &crossbeam_channel::Sender<Event>) -> Result<()> {
        let headers = remote_data::authentication_headers(self.api_token.as_str())?;
        let url = self.endpoint.join("api/v3/session/udp")?;
        let sender = sender.clone();
        let client = client.clone();
        thread::spawn(move || {
            tracing::debug!(?headers, ?url, "list sessions");

            let fetch_res = client
                .get(url)
                .timeout(std::time::Duration::from_secs(30))
                .headers(headers)
                .send()
                .map(|res| (res.status(), res.json::<serde_json::Value>()));

            let evt = match fetch_res {
                Ok((status, Ok(json))) if status.is_success() => {
                    Event::FetchListSessions(remote_data::Event::Response(json))
                }
                Ok((status, Ok(json))) => {
                    let e = remote_data::CustomError {
                        reqw_err: None,
                        status: Some(status),
                        value: Some(json),
                    };
                    Event::FetchListSessions(remote_data::Event::Error(e))
                }
                Ok((status, Err(e))) => {
                    let e = remote_data::CustomError {
                        reqw_err: Some(e),
                        status: Some(status),
                        value: None,
                    };
                    Event::FetchListSessions(remote_data::Event::Error(e))
                }
                Err(e) => {
                    let e = remote_data::CustomError {
                        reqw_err: Some(e),
                        status: None,
                        value: None,
                    };
                    Event::FetchListSessions(remote_data::Event::Error(e))
                }
            };
            sender.send(evt)
        });
        Ok(())
    }
}
