## `sigchld` [![Actions Status](https://github.com/oconnor663/sigchld.rs/workflows/tests/badge.svg)](https://github.com/oconnor663/sigchld.rs/actions) [![crates.io](https://img.shields.io/crates/v/sigchld.svg)](https://crates.io/crates/sigchld) [![docs.rs](https://docs.rs/sigchld/badge.svg)](https://docs.rs/sigchld)

This is a low-level utility for child process management. Unix doesn't provide a portable\* API
for waiting for a child process to exit **with a timeout**. The closest thing is waiting for
the `SIGCHLD` signal to be delivered, but Unix signal handling is quite complicated and
error-prone. This crate implements `SIGCHLD` handling (using [`signal_hook`] internally for
compatibility with other signal handling libraries) and allows any number of threads to wait
for that signal, with an optional timeout.

Note that `SIGCHLD` indicates that _any_ child process has exited, but there's no (100%
reliable) way to know _which_ child it was. You generally need to [poll your child
process][try_wait] in a loop, and wait again if it hasn't exited yet. Most applications will
want to use a higher-level API that does this loop internally; I'll list such crates here as
they're implemented.

\* Linux supports `signalfd`, but there's no equivalent on e.g. macOS.

## Example

```rust
let mut waiter = sigchld::Waiter::new()?;
// Any SIGCHLD after this point will be buffered by the Waiter.
let mut child = std::process::Command::new("sleep").arg("1").spawn()?;
// Block until *any* child exits. See also `wait_timeout` and `wait_deadline`.
waiter.wait()?;
// There's only one child process in this example, so we know that it exited. But in general
// we might not know which child woke us up, and then we'd need to wait and check in a loop.
// See the Waiter examples.
assert!(child.try_wait()?.is_some(), "sleep has exited");
```

[`signal_hook`]: https://docs.rs/signal-hook
[try_wait]: https://doc.rust-lang.org/std/process/struct.Child.html#method.try_wait
