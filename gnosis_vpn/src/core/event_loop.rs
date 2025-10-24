use tokio::sync::mpsc;

use std::fmt::{self, Display};

pub struct EventLoop {
    // Internal state of the event loop
}

pub enum Results {
    Foobar,
}

impl EventLoop {
    pub async fn start(&mut self, sender: mpsc::Sender<Results>) {
        // Event loop logic goes here
        // For example, sending a Foobar result
        let _ = sender.send(Results::Foobar).await;
    }
}

impl Display for Results {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Results::Foobar => write!(f, "Foobar Result"),
        }
    }
}
