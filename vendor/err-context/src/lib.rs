#![doc(
    html_root_url = "https://docs.rs/err-context/0.1.0/err-context/",
    test(attr(deny(warnings)))
)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! Lightweight error context manipulation.
//!
//! Oftentimes, when error is being reported to the user, the lowest-level cause of the original
//! error is not very interesting (eg. „No such file or directory“). It is not clear what file it
//! should have been, why the program needed that file and what the consequences of failing to find
//! it are.
//!
//! This library allows to wrap the low-level error into layers of context easily and examine such
//! multi-layer libraries. Depending on preferences of the developer, the story of the whole error
//! might then be found either in the outer layer, or the whole chain may be needed.
//!
//! There are other libraries with similar aim, though none seemed to fit very well. Most of them
//! have unergonomic API. This API is modeled after the [`failure`] crate (which has simple and
//! powerful API), but uses the [`Error`] trait in standard library.
//!
//! # Compatibility
//!
//! By using the trait and [`AnyError`] type alias, the library is compatible with any future or
//! past version of self or any other error handling library that operates on the [`Error`] trait.
//! To avoid dependencies on specific version of this library, downstream crates are advised to not
//! reexport types from here with the sole exception of the [`AnyError`] alias (as it is alias, it
//! can be re-created independently by as many libraries and is compatible). Downstream libraries
//! are, of course, free to expose their own errors.
//!
//! # Using the library
//!
//! Besides the [`AnyError`] alias, users of the library probably don't want to use any of the
//! present types here *directly*. Instead, certain traits can be imported and all errors, boxed
//! errors and results containing them get additional methods.
//!
//! The methods can be found on the [`ErrorExt`] and [`ResultExt`] traits.
//!
//! ## Producing errors
//!
//! The lowest level error comes from somewhere else ‒ it may be type provided by some other crate,
//! an error implemented manually or generated with help of some other crate (the [`err-derive`]
//! crate offers derive macro similar to the one of [`failure`], but for standard errors ‒
//! combination of these two crates provide almost all the functionality of [`failure`]).
//!
//! Then, as the error bubbles up, it can be enriched by additional information using the
//! [`.context`][ErrorExt::context] and [`.with_context`][ResultExt::context] methods.
//!
//! ```
//! use std::error::Error;
//! use std::io::Read;
//! use std::fmt::{Display, Formatter, Result as FmtResult};
//! use std::fs::File;
//! use std::path::Path;
//!
//! use err_context::AnyError;
//! use err_context::prelude::*;
//!
//! // An example error. You'd implement it manually like this, or use something else, like
//! // err-derive, to generate it.
//! #[derive(Debug)]
//! struct BrokenImage;
//!
//! impl Display for BrokenImage {
//!     fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
//!         write!(fmt, "The image is broken")
//!     }
//! }
//!
//! impl Error for BrokenImage {}
//!
//! #[derive(Clone, Debug)]
//! struct Image;
//!
//! impl Image {
//!     fn parse(_: Vec<u8>) -> Result<Self, BrokenImage> {
//!         // This is an example. We didn't really implement this.
//!         Err(BrokenImage)
//!     }
//! }
//!
//! fn read_image<P: AsRef<Path>>(path: P) -> Result<Image, AnyError> {
//!     let mut file = File::open(&path)
//!         .with_context(|_| format!("Couldn't open file {}", path.as_ref().display()))?;
//!     let mut data = Vec::new();
//!     file.read_to_end(&mut data)
//!         .with_context(|_| format!("File {} couldn't be read", path.as_ref().display()))?;
//!     let image = Image::parse(data)
//!         .with_context(|_| format!("Image in file {} is corrupt", path.as_ref().display()))?;
//!     Ok(image)
//! }
//!
//! # fn main() { read_image("/dev/null").unwrap_err(); }
//! ```
//!
//! Note that the type of the error produced by any of these methods doesn't carry any useful
//! information. Therefore this library should be used only at places where the error is meant for
//! printing out to user or some other handling in uniform manner. Libraries providing building
//! blocks might want to implement their own typed errors, with possibly usefully typed layers.
//!
//! ## Consuming the errors
//!
//! These kinds of errors are usually meant for the user. The outer layer's [`Display`] contains
//! only its own context, eg:
//!
//! ```
//! # use err_context::prelude::*;
//! let inner = std::io::Error::last_os_error();
//! let outer = inner.context("Some error");
//! assert_eq!("Some error", outer.to_string());
//! ```
//!
//! If you're interested in all the layers, they can be iterated (this simply calls the
//! [`source`][Error::source] method until it gets to the bottom).
//!
//! ```
//! # use err_context::prelude::*;
//! let inner = std::io::Error::last_os_error();
//! let outer = inner.context("Some error");
//! // This will print
//! //   Some error
//! //   success (or whatever the last io error was)
//! for e in outer.chain() {
//!     println!("{}", e);
//! }
//! ```
//!
//! Or, more conveniently can be printed as a single string:
//!
//! ```
//! # use err_context::prelude::*;
//! let inner = std::io::Error::last_os_error();
//! let outer = inner.context("Some error");
//! // Will print something like:
//! // Some error: success
//! println!("{}", outer.display(", "));
//! ```
//!
//! Despite being mostly aimed for human output, the chain still can be examined to an extend. In
//! particular, it is possible to look for the outermost error in the chain of specific type. This
//! will find the inner error.
//!
//! ```
//! # use err_context::prelude::*;
//! let inner = std::io::Error::last_os_error();
//! let outer = inner.context("Some error");
//! assert!(outer.find_source::<std::io::Error>().is_some());
//! ```
//!
//! [`failure`]: https://crates.io/crates/failure
//! [`err-derive`]: https://crates.io/crates/err-derive

use std::error::Error;
use std::fmt::{Debug, Display, Formatter, Result as FmtResult};
use std::mem;

/// A type alias for boxed errors.
///
/// By convention, errors should be Send and Sync (because they are usually just dumb values).
/// Everything in the crate should work even with similar types without these bounds.
///
/// Re-defining the type alias (or using it without the type alias) works too. This is just
/// convenience and self-explanation of code.
///
/// It is possible a similar type alias will eventually get added to the standard library. If so,
/// the crate will be automatically compatible with that too.
pub type AnyError = Box<dyn Error + Send + Sync>;

/// An iterator through the error chain.
///
/// Produced by the [chain][ErrorExt::chain] method.
#[derive(Copy, Clone, Debug)]
pub struct Chain<'a>(Option<&'a (dyn Error + 'static)>);

impl<'a> Iterator for Chain<'a> {
    type Item = &'a (dyn Error + 'static);
    fn next(&mut self) -> Option<Self::Item> {
        let current = self.0.take();
        if let Some(current) = current {
            self.0 = current.source();
        }
        current
    }
}

/// A display implementation for formatting separated layers of an error.
///
/// Produced by the [`display`][ErrorExt::display] method.
#[derive(Copy, Clone, Debug)]
pub struct DisplayChain<'a, S> {
    chain: Chain<'a>,
    separator: S,
}

impl<S: Display> Display for DisplayChain<'_, S> {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        let mut started = false;
        // Note: iteration takes &mut, but Chain is copy, so we iterate over a copy.
        for level in self.chain {
            if mem::replace(&mut started, true) {
                write!(fmt, "{}", self.separator)?;
            }
            write!(fmt, "{}", level)?;
        }
        Ok(())
    }
}

/// Additional level of context, wrapping some error inside itself.
///
/// This is produced by the [`context`][ErrorExt::context] and implements the additional (outer)
/// layer in the chain. Any number of contexts can be chained together.
#[derive(Copy, Clone, Debug)]
pub struct Context<M, E> {
    msg: M,
    inner: E,
}

impl<M: Display, E> Display for Context<M, E> {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        self.msg.fmt(fmt)
    }
}

impl<M: Debug + Display, E: Error + 'static> Error for Context<M, E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.inner)
    }
}

impl<M, E> Context<M, E> {
    /// A direct constructor for the context.
    ///
    /// More usually created by the [`context`][ErrorExt::context], but allowing for construction
    /// directly without importing the trait.
    pub fn new(msg: M, error: E) -> Self {
        Self { msg, inner: error }
    }

    /// Extracts the inner error, peeling off the outer layer.
    pub fn into_inner(self) -> E {
        self.inner
    }
}

/// Additional context for boxed errors.
///
/// This has the same purpose and the same functionality as [`Context`]. It is a separate type for
/// technical reasons in implementation.
#[derive(Debug)]
pub struct BoxContext<M, E: ?Sized> {
    msg: M,
    inner: Box<E>,
}

impl<M, E: ?Sized> BoxContext<M, E> {
    /// Direct construction of the context.
    pub fn new(msg: M, error: Box<E>) -> Self {
        Self { msg, inner: error }
    }

    /// Extracts the inner error, peeling off the outer layer.
    pub fn into_inner(self) -> Box<E> {
        self.inner
    }
}

impl<M: Display, E: ?Sized> Display for BoxContext<M, E> {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        self.msg.fmt(fmt)
    }
}

macro_rules! impl_err {
    ($ty: ty) => {
        impl<M: Debug + Display> Error for BoxContext<M, $ty> {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                Some(&*self.inner)
            }
        }
    };
}

impl_err!(dyn Error + Send + Sync);
impl_err!(dyn Error + Send);
impl_err!(dyn Error + Sync);
impl_err!(dyn Error);

#[cfg(feature = "failure")]
impl<M> Context<M, failure::Compat<failure::Error>> {
    /// Constructor of context from a failure's [`Error`][failure::Error].
    ///
    /// This is a compatibility constructor, for wrapping the error of failure. It is enabled by
    /// the `failure` feature.
    ///
    /// # Warning
    ///
    /// The compatibility layer has no way to access the original causes of the failure. Therefore,
    /// the inner layers of the provided failure will be lost and the failure will be considered
    /// the innermost level.
    pub fn from_failure(msg: M, failure: failure::Error) -> Self {
        Self::new(msg, failure.compat())
    }
}

#[cfg(feature = "failure")]
impl<M, F: failure::Fail> Context<M, failure::Compat<F>> {
    /// Constructor of context from a failure's [`Fail`][failure::Fail].
    ///
    /// This is a compatibility constructor, for wrapping failure. It is enabled by the `failure`
    /// feature.
    ///
    /// # Warning
    ///
    /// The compatibility layer has no way to access the original causes of the failure. Therefore,
    /// the inner layers of the provided failure will be lost and the failure will be considered
    /// the innermost level.
    pub fn from_fail(msg: M, failure: F) -> Self {
        Self::new(msg, failure.compat())
    }
}

/// Extension trait for the [`Error`] trait.
///
/// This adds additional methods to all the [`Error`] types. See the [crate
/// documentation](index.html) for examples and general principles.
///
/// Note that usually this trait is not imported directly, but through the [`prelude`].
pub trait ErrorExt: private::Sealed + Sized {
    /// Wraps the error into another layer of context.
    ///
    /// The produced error will have one more layer. The outer layer will have the provided message
    /// as its [`Display`] implementation.
    fn context<M: Display>(self, msg: M) -> Context<M, Self>;

    /// Iterates over all the layers of the error.
    ///
    /// The first entry will be the outer layer (`self`), the last one the lowest-level/innermost
    /// source. Therefore this iterator is never empty. It iterates by reference.
    fn chain(&self) -> Chain<'_>
    where
        Self: 'static;

    /// Looks for an outermost error of the given type.
    ///
    /// This is combination of iteration and downcasting of the errors. The returned value is
    /// reference to the outermost error layer that matches given type, as the given type.
    ///
    /// The type of the context layers is often very uninteresting, but the code might want to find
    /// some specific error in there. This allows skipping the unknown number of human-readable
    /// „comments“ and get to the facts. Note that it doesn't have to be the lowest-level one ‒
    /// even properly typed errors can have their sources.
    fn find_source<T: Error + 'static>(&self) -> Option<&T>
    where
        Self: 'static,
    {
        self.chain().find_map(|e| e.downcast_ref::<T>())
    }

    /// Returns a [`Display`] representation of the whole chain of errors.
    ///
    /// This can be used to output the whole chain (as opposed to just outputting the error
    /// directly). The layers are separated by the provided `separator`.
    fn display<S: Display>(&self, separator: S) -> DisplayChain<'_, S>
    where
        Self: 'static,
    {
        DisplayChain {
            chain: self.chain(),
            separator,
        }
    }
}

impl<E: Error> private::Sealed for E {}

impl<E: Error> ErrorExt for E {
    fn context<M: Display>(self, msg: M) -> Context<M, Self> {
        Context::new(msg, self)
    }
    fn chain(&self) -> Chain<'_>
    where
        Self: 'static,
    {
        Chain(Some(self))
    }
}

/// An equivalent of [`ErrorExt`] for boxed errors.
///
/// This is effectively the same trait as [`ErrorExt`], but for boxed errors. It exists separately
/// purely for implementation reasons.
pub trait BoxedErrorExt<E: ?Sized>: private::BoxedSealed + Sized {
    /// Equivalent of [`ErrorExt::context`].
    fn context<M: Display>(self, msg: M) -> BoxContext<M, E>;
    /// Equivalent of [`ErrorExt::chain`].
    fn chain(&self) -> Chain<'_>;
    /// Equivalent of [`ErrorExt::find_source`].
    fn find_source<T: Error + 'static>(&self) -> Option<&T> {
        self.chain().find_map(|e| e.downcast_ref::<T>())
    }
    /// Equivalent of [`ErrorExt::display`].
    fn display<S: Display>(&self, separator: S) -> DisplayChain<'_, S> {
        DisplayChain {
            chain: self.chain(),
            separator,
        }
    }
}

macro_rules! impl_any_error {
    ($ty: ty) => {
        impl private::BoxedSealed for Box<$ty> {}
        impl BoxedErrorExt<$ty> for Box<$ty> {
            fn context<M: Display>(self, msg: M) -> BoxContext<M, $ty> {
                BoxContext::new(msg, self)
            }
            fn chain(&self) -> Chain<'_> {
                Chain(Some(&**self))
            }
        }
    };
}

impl_any_error!(dyn Error + Send + Sync);
impl_any_error!(dyn Error + Send);
impl_any_error!(dyn Error + Sync);
impl_any_error!(dyn Error);

/// Extension traits for results.
///
/// This provides method to enrich the error in the result with additional context. See the general
/// principles and examples at the [crate level documentation](index.html).
///
/// Usually, this trait isn't imported directly, but through the [`prelude`].
pub trait ResultExt<T, E>: private::ResultSealed + Sized {
    /// Wraps the error in another layer of context.
    ///
    /// If the result is success, this does nothing. If it is error, it wraps the error in another
    /// layer, in the same way as calling [`.context`][ErrorExt::context] on that error itself
    /// would.
    ///
    /// If the construction of the message is expensive, consider using
    /// [`with_context`][ResultExt::with_context].
    fn context<M>(self, msg: M) -> Result<T, Context<M, E>>
    where
        M: Display;

    /// Wraps the error in another layer of context.
    ///
    /// This works in a similar way as [`context`][ResultExt::context]. However, the closure to
    /// construct the context is called only in case the result is `Err`, avoiding the construction
    /// cost in the success case.
    ///
    /// As the common usage goes, string literal can be passed directly with
    /// [`context`][ResultExt::context], but calling `format!` to construct the message would be
    /// better done with this method.
    fn with_context<F, M>(self, f: F) -> Result<T, Context<M, E>>
    where
        F: FnOnce(&E) -> M,
        M: Display;
}

impl<T, E: Error> private::ResultSealed for Result<T, E> {}

impl<T, E: Error> ResultExt<T, E> for Result<T, E> {
    fn context<M>(self, msg: M) -> Result<T, Context<M, E>>
    where
        M: Display,
    {
        self.map_err(|e| e.context(msg))
    }

    fn with_context<F, M>(self, f: F) -> Result<T, Context<M, E>>
    where
        F: FnOnce(&E) -> M,
        M: Display,
    {
        self.map_err(|e| {
            let msg = f(&e);
            e.context(msg)
        })
    }
}

/// A [`ResultExt`] equivalent for boxed errors.
///
/// This trait serves the same purpose and acts in the same ways as the [`ResultExt`], so refer to
/// that for details. It exists merely for implementation purposes.
pub trait BoxedResultExt<T, E: ?Sized>: private::BoxedResultSealed {
    /// A [`ResultExt::context`] equivalent.
    fn context<M>(self, msg: M) -> Result<T, BoxContext<M, E>>
    where
        M: Display;

    /// A [`ResultExt::with_context`] equivalent.
    fn with_context<F, M>(self, f: F) -> Result<T, BoxContext<M, E>>
    where
        F: FnOnce(&Box<E>) -> M,
        M: Display;
}

macro_rules! any_result_impl {
    ($ty: ty) => {
        impl<T> private::BoxedResultSealed for Result<T, Box<$ty>> {}
        impl<T> BoxedResultExt<T, $ty> for Result<T, Box<$ty>> {
            fn context<M>(self, msg: M) -> Result<T, BoxContext<M, $ty>>
            where
                M: Display,
            {
                self.map_err(|e| e.context(msg))
            }

            fn with_context<F, M>(self, f: F) -> Result<T, BoxContext<M, $ty>>
            where
                F: FnOnce(&Box<$ty>) -> M,
                M: Display,
            {
                self.map_err(|e| {
                    let msg = f(&e);
                    e.context(msg)
                })
            }
        }
    };
}

any_result_impl!(dyn Error + Send + Sync);
any_result_impl!(dyn Error + Send);
any_result_impl!(dyn Error + Sync);
any_result_impl!(dyn Error);

/// A module with reexports to wildcard-import all relevant traits.
///
/// The library adds methods to existing types by extension traits. For them to work, they need to
/// be in scope. It is more convenient to import them all from prelude instead of individually.
///
/// ```
/// # #[allow(unused_imports)]
/// use err_context::prelude::*;
/// ```
///
/// Only the traits are imported and they are imported anonymously (so their names can't clash with
/// anything, but they also can't be referred directly).
pub mod prelude {
    pub use crate::BoxedErrorExt as _;
    pub use crate::BoxedResultExt as _;
    pub use crate::ErrorExt as _;
    pub use crate::ResultExt as _;
}

mod private {
    // Trick to prevent others implementing our extension traits. This allows us adding new methods
    // in the future without breaking API.
    pub trait Sealed {}
    pub trait BoxedSealed {}
    pub trait ResultSealed {}
    pub trait BoxedResultSealed {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error as IoError, Read};

    fn _context_error() -> impl Error {
        IoError::last_os_error().context("Hello")
    }

    fn _context_any_error() -> impl Error {
        let e: AnyError = IoError::last_os_error().into();
        e.context(42)
    }

    fn _context_result() -> Result<(), AnyError> {
        let mut buf = [0];
        std::io::stdin()
            .read(&mut buf)
            .context("Failed to read line")?;
        Ok(())
    }

    fn _double_context() -> Result<(), AnyError> {
        _context_result().with_context(|e| format!("Hey: {}", e))?;
        Ok(())
    }

    #[derive(Copy, Clone, Debug)]
    struct Dummy;

    impl Display for Dummy {
        fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
            write!(fmt, "Dummy error!")
        }
    }

    impl Error for Dummy {}

    fn get_chain() -> AnyError {
        Dummy.context("Sorry").into()
    }

    fn get_boxed() -> AnyError {
        Dummy.into()
    }

    fn get_boxed_chain() -> AnyError {
        let a = get_boxed().context("Sorry");
        a.into()
    }

    #[test]
    fn iter_chain() {
        assert_eq!(1, Dummy.chain().count());
        assert_eq!(2, get_chain().chain().count());
    }

    #[test]
    fn find_dummy() {
        assert!(Dummy.find_source::<Dummy>().is_some());
        assert!(get_chain().find_source::<Dummy>().is_some());
        assert!(get_boxed_chain().find_source::<Dummy>().is_some());
        assert!(get_chain().find_source::<IoError>().is_none());
    }

    #[test]
    fn display_chain() {
        let chain = get_chain();
        assert_eq!("Sorry", chain.to_string());
        assert_eq!("Sorry: Dummy error!", chain.display(": ").to_string());
    }
}
