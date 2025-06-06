# err-context

[![Travis Build Status](https://api.travis-ci.org/vorner/err-context.png?branch=master)](https://travis-ci.org/vorner/err-context)

This library allows one to attach context (layers) onto errors with minimal
amount of work and analyze such multi-layer errors.

The API and working is heavily influenced by the
[failure](https://crates.io/crates/failure) crate. However, only the part that
works with contexts/layers is present in here (for generating error types, see
the [err-derive](https://crates.io/crates/err-derive), which implements the
derive part of functionality). Furthermore, this library works with `std`
errors.

Read [the documentation](https://docs.rs/err-context) before using.

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms
or conditions.
