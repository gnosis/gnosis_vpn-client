/// Placeholder routing actor for non-Linux platforms.
/// No killswitch functionality is available; messages are never sent.

pub enum Msg {}

pub(super) struct Actor;

impl Actor {
    pub(super) fn new() -> Self {
        Actor
    }

    pub(super) fn handle(&mut self, _msg: Msg) {}

    pub(super) fn teardown(&mut self) {}
}
