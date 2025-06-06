# byte_string [![Build Status](https://travis-ci.org/FraGag/byte_string.svg?branch=master)](https://travis-ci.org/FraGag/byte_string)

The `byte_string` crate provides two types: `ByteStr` and `ByteString`.
Both types provide a `Debug` implementation
that outputs the slice using the Rust byte string syntax.
`ByteStr` wraps a byte slice (`[u8]`).
`ByteString` wraps a vector of bytes (`Vec<u8>`).

For example:

```rust
extern crate byte_string;

use byte_string::ByteStr;

fn main() {
    let s = b"Hello, world!";
    let bs = ByteStr::new(s);
    assert_eq!(format!("{:?}", bs), "b\"Hello, world!\"");
}
```

`ByteStr` is an unsized type, as `[u8]` is.
`ByteStr::new()` returns a `&ByteStr`
and `ByteStr::new_mut()` returns a `&mut ByteStr`.

`ByteStr` and `ByteString` are meant to be used as an implementation detail.
You should generally avoid exposing a `ByteStr` or a `ByteString`
as part of a struct or enum;
prefer exposing the underlying slice or vector instead.
However, `ByteStr` and `ByteString` implement many traits, including derivable traits,
which makes them suitable for use as a private member of a struct or enum.

## License

<b>byte_string</b> is licensed
under the terms of both the [MIT license][license-mit]
and the [Apache License, version 2.0][license-apache].

[license-mit]: LICENSE-MIT
[license-apache]: LICENSE-APACHE
