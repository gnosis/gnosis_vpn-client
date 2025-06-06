//! The `byte_string` crate provides two types: `ByteStr` and `ByteString`.
//! Both types provide a `Debug` implementation
//! that outputs the slice using the Rust byte string syntax.
//! `ByteStr` wraps a byte slice (`[u8]`).
//! `ByteString` wraps a vector of bytes (`Vec<u8>`).
//!
//! For example:
//!
//! ```
//! extern crate byte_string;
//!
//! use byte_string::ByteStr;
//!
//! fn main() {
//!     let s = b"Hello, world!";
//!     let bs = ByteStr::new(s);
//!     assert_eq!(format!("{:?}", bs), "b\"Hello, world!\"");
//! }
//! ```
//!
//! `ByteStr` is an unsized type, as `[u8]` is.
//! `ByteStr::new()` returns a `&ByteStr`
//! and `ByteStr::new_mut()` returns a `&mut ByteStr`.
//!
//! `ByteStr` and `ByteString` are meant to be used as an implementation detail.
//! You should generally avoid exposing a `ByteStr` or a `ByteString`
//! as part of a struct or enum;
//! prefer exposing the underlying slice or vector instead.
//! However, `ByteStr` and `ByteString` implement many traits, including derivable traits,
//! which makes them suitable for use as a private member of a struct or enum.

#![warn(missing_docs)]

use std::borrow::{Borrow, BorrowMut};
use std::fmt::{Debug, Error, Formatter};
use std::iter::FromIterator;
use std::mem;
use std::ops::{Deref, DerefMut};

/// Wraps a byte slice and provides a `Debug` implementation
/// that outputs the slice using the Rust byte string syntax (e.g. `b"abc"`).
#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteStr(pub [u8]);

/// Wraps a vector of bytes and provides a `Debug` implementation
/// that outputs the slice using the Rust byte string syntax (e.g. `b"abc"`).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteString(pub Vec<u8>);

impl ByteStr {
    /// Converts an immutable byte slice to an immutable `ByteStr` reference.
    pub fn new(s: &[u8]) -> &ByteStr {
        unsafe { mem::transmute(s) }
    }

    /// Converts a mutable byte slice to a mutable `ByteStr` reference.
    pub fn new_mut(s: &mut [u8]) -> &mut ByteStr {
        unsafe { mem::transmute(s) }
    }
}

impl<'a> From<&'a [u8]> for &'a ByteStr {
    fn from(s: &[u8]) -> &ByteStr {
        ByteStr::new(s)
    }
}

impl<'a> From<&'a mut [u8]> for &'a mut ByteStr {
    fn from(s: &mut [u8]) -> &mut ByteStr {
        ByteStr::new_mut(s)
    }
}

impl<'a> From<&'a ByteStr> for &'a [u8] {
    fn from(s: &ByteStr) -> &[u8] {
        &s.0
    }
}

impl<'a> From<&'a mut ByteStr> for &'a mut [u8] {
    fn from(s: &mut ByteStr) -> &mut [u8] {
        &mut s.0
    }
}

impl AsRef<[u8]> for ByteStr {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<ByteStr> for [u8] {
    fn as_ref(&self) -> &ByteStr {
        ByteStr::new(self)
    }
}

impl AsMut<[u8]> for ByteStr {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl AsMut<ByteStr> for [u8] {
    fn as_mut(&mut self) -> &mut ByteStr {
        ByteStr::new_mut(self)
    }
}

impl PartialEq<[u8]> for ByteStr {
    fn eq(&self, other: &[u8]) -> bool {
        &self.0 == other
    }
}

impl PartialEq<ByteStr> for [u8] {
    fn eq(&self, other: &ByteStr) -> bool {
        self == &other.0
    }
}

impl Deref for ByteStr {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl DerefMut for ByteStr {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl<'a> Default for &'a ByteStr {
    fn default() -> &'a ByteStr {
        ByteStr::new(&[])
    }
}

impl<'a> Default for &'a mut ByteStr {
    fn default() -> &'a mut ByteStr {
        ByteStr::new_mut(&mut [])
    }
}

impl<'a> IntoIterator for &'a ByteStr {
    type Item = &'a u8;
    type IntoIter = std::slice::Iter<'a, u8>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a mut ByteStr {
    type Item = &'a mut u8;
    type IntoIter = std::slice::IterMut<'a, u8>;

    fn into_iter(self) -> Self::IntoIter {
        (&mut self.0).into_iter()
    }
}

impl Debug for ByteStr {
    fn fmt(&self, f: &mut Formatter) -> Result<(), Error> {
        try!(write!(f, "b\""));

        for &byte in self {
            for ch in std::ascii::escape_default(byte) {
                try!(write!(f, "{}", ch as char));
            }
        }

        write!(f, "\"")
    }
}

impl ByteString {
    /// Moves a vector of bytes to a new `ByteString`.
    pub fn new(s: Vec<u8>) -> ByteString {
        ByteString(s)
    }
}

impl From<Vec<u8>> for ByteString {
    fn from(s: Vec<u8>) -> ByteString {
        ByteString::new(s)
    }
}

impl From<ByteString> for Vec<u8> {
    fn from(s: ByteString) -> Vec<u8> {
        s.0
    }
}

impl AsRef<Vec<u8>> for ByteString {
    fn as_ref(&self) -> &Vec<u8> {
        &self.0
    }
}

impl AsRef<[u8]> for ByteString {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl AsMut<Vec<u8>> for ByteString {
    fn as_mut(&mut self) -> &mut Vec<u8> {
        &mut self.0
    }
}

impl AsMut<[u8]> for ByteString {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl Borrow<ByteStr> for ByteString {
    fn borrow(&self) -> &ByteStr {
        ByteStr::new(&self.0)
    }
}

impl Borrow<Vec<u8>> for ByteString {
    fn borrow(&self) -> &Vec<u8> {
        &self.0
    }
}

impl Borrow<[u8]> for ByteString {
    fn borrow(&self) -> &[u8] {
        &self.0
    }
}

impl BorrowMut<ByteStr> for ByteString {
    fn borrow_mut(&mut self) -> &mut ByteStr {
        ByteStr::new_mut(&mut self.0)
    }
}

impl BorrowMut<Vec<u8>> for ByteString {
    fn borrow_mut(&mut self) -> &mut Vec<u8> {
        &mut self.0
    }
}

impl BorrowMut<[u8]> for ByteString {
    fn borrow_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

impl PartialEq<Vec<u8>> for ByteString {
    fn eq(&self, other: &Vec<u8>) -> bool {
        self.0 == *other
    }
}

impl PartialEq<[u8]> for ByteString {
    fn eq(&self, other: &[u8]) -> bool {
        self.0 == other
    }
}

impl PartialEq<ByteString> for Vec<u8> {
    fn eq(&self, other: &ByteString) -> bool {
        self == &other.0
    }
}

impl PartialEq<ByteString> for [u8] {
    fn eq(&self, other: &ByteString) -> bool {
        self == &other.0[..]
    }
}

impl Deref for ByteString {
    type Target = Vec<u8>;

    fn deref(&self) -> &Vec<u8> {
        &self.0
    }
}

impl DerefMut for ByteString {
    fn deref_mut(&mut self) -> &mut Vec<u8> {
        &mut self.0
    }
}

impl Default for ByteString {
    fn default() -> ByteString {
        ByteString::new(vec![])
    }
}

impl FromIterator<u8> for ByteString {
    fn from_iter<I>(iter: I) -> ByteString
        where I: IntoIterator<Item=u8>
    {
        ByteString::new(Vec::from_iter(iter))
    }
}

impl<'a> IntoIterator for ByteString {
    type Item = u8;
    type IntoIter = std::vec::IntoIter<u8>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a ByteString {
    type Item = &'a u8;
    type IntoIter = std::slice::Iter<'a, u8>;

    fn into_iter(self) -> Self::IntoIter {
        (&self.0).into_iter()
    }
}

impl<'a> IntoIterator for &'a mut ByteString {
    type Item = &'a mut u8;
    type IntoIter = std::slice::IterMut<'a, u8>;

    fn into_iter(self) -> Self::IntoIter {
        (&mut self.0).into_iter()
    }
}

impl Debug for ByteString {
    fn fmt(&self, f: &mut Formatter) -> Result<(), Error> {
        // Delegate to ByteStr's implementation
        Debug::fmt(Borrow::<ByteStr>::borrow(self), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &'static str = "b\"\"";
    const ALL_BYTES: &'static str = concat!("b\"",
        "\\x00\\x01\\x02\\x03\\x04\\x05\\x06\\x07\\x08\\t\\n\\x0b\\x0c\\r\\x0e\\x0f",
        "\\x10\\x11\\x12\\x13\\x14\\x15\\x16\\x17\\x18\\x19\\x1a\\x1b\\x1c\\x1d\\x1e\\x1f",
        " !\\\"#$%&\\\'()*+,-./",
        "0123456789:;<=>?",
        "@ABCDEFGHIJKLMNO",
        "PQRSTUVWXYZ[\\\\]^_",
        "`abcdefghijklmno",
        "pqrstuvwxyz{|}~\\x7f",
        "\\x80\\x81\\x82\\x83\\x84\\x85\\x86\\x87\\x88\\x89\\x8a\\x8b\\x8c\\x8d\\x8e\\x8f",
        "\\x90\\x91\\x92\\x93\\x94\\x95\\x96\\x97\\x98\\x99\\x9a\\x9b\\x9c\\x9d\\x9e\\x9f",
        "\\xa0\\xa1\\xa2\\xa3\\xa4\\xa5\\xa6\\xa7\\xa8\\xa9\\xaa\\xab\\xac\\xad\\xae\\xaf",
        "\\xb0\\xb1\\xb2\\xb3\\xb4\\xb5\\xb6\\xb7\\xb8\\xb9\\xba\\xbb\\xbc\\xbd\\xbe\\xbf",
        "\\xc0\\xc1\\xc2\\xc3\\xc4\\xc5\\xc6\\xc7\\xc8\\xc9\\xca\\xcb\\xcc\\xcd\\xce\\xcf",
        "\\xd0\\xd1\\xd2\\xd3\\xd4\\xd5\\xd6\\xd7\\xd8\\xd9\\xda\\xdb\\xdc\\xdd\\xde\\xdf",
        "\\xe0\\xe1\\xe2\\xe3\\xe4\\xe5\\xe6\\xe7\\xe8\\xe9\\xea\\xeb\\xec\\xed\\xee\\xef",
        "\\xf0\\xf1\\xf2\\xf3\\xf4\\xf5\\xf6\\xf7\\xf8\\xf9\\xfa\\xfb\\xfc\\xfd\\xfe\\xff",
        "\"");

    #[test]
    fn debug_bytestr_empty() {
        let bytes = [];
        let bs = ByteStr::new(&bytes);
        let result = format!("{:?}", bs);
        assert_eq!(result, EMPTY);
    }

    #[test]
    fn debug_bytestr() {
        let bytes = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
            0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F,
            0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F,
            0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F,
            0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F,
            0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F,
            0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F,
            0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F,
            0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F,
            0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF,
            0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF,
            0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF,
            0xD0, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF,
            0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF,
            0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE, 0xFF,
        ];
        let bs = ByteStr::new(&bytes);
        let result = format!("{:?}", bs);
        assert_eq!(result, ALL_BYTES);
    }

    #[test]
    fn debug_bytestring_empty() {
        let bytes = vec![];
        let bs = ByteString::new(bytes);
        let result = format!("{:?}", bs);
        assert_eq!(result, EMPTY);
    }

    #[test]
    fn debug_bytestring() {
        let bytes = vec![
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
            0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F,
            0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F,
            0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F,
            0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F,
            0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F,
            0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F,
            0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F,
            0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F,
            0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF,
            0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF,
            0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF,
            0xD0, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF,
            0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF,
            0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE, 0xFF,
        ];
        let bs = ByteString::new(bytes);
        let result = format!("{:?}", bs);
        assert_eq!(result, ALL_BYTES);
    }
}
