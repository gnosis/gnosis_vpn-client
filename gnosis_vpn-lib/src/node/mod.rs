#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// Connection has been fully established and ping tested
    Connected,
    /// Currently not connected
    Disconnected,
    /// Connection is broken and should be dismantled
    Broken,
    /// Connection has reached final state
    Dismantled,
}

#[derive(Clone, Debug)]
pub struct Node {
    // reuse http client
    client: blocking::Client,

    // dynamic runtime data
    backoff: BackoffState,

    // static input data
    entry_node: EntryNode,
    sender: crossbeam_channel::Sender<Event>,
}
