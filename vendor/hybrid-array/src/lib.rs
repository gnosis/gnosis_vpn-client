#![no_std]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/RustCrypto/meta/master/logo.svg",
    html_favicon_url = "https://raw.githubusercontent.com/RustCrypto/meta/master/logo.svg"
)]
#![warn(
    clippy::arithmetic_side_effects,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::checked_conversions,
    clippy::from_iter_instead_of_collect,
    clippy::missing_errors_doc,
    clippy::mod_module_files,
    clippy::implicit_saturating_sub,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unwrap_used,
    missing_docs,
    missing_debug_implementations,
    rust_2018_idioms,
    trivial_casts,
    trivial_numeric_casts,
    unused_lifetimes,
    unused_qualifications
)]

//! ## Features
//!
//! This crate exposes the following feature flags. The default is NO features.
//!
//! - `bytemuck`: impls the `Pod` and `Zeroable` traits
//! - `serde`: impls the `Deserialize` and `Serialize` traits for `Array`
//! - `zeroize`: impls [`Zeroize`](https://docs.rs/zeroize/latest/zeroize/trait.Zeroize.html) for `Array<T: Zeroize, U>`
//!
//! ## Usage
//!
//! The two core types in this crate are as follows:
//!
//! - [`Array<T, U>`]: wrapper for `[T; N]` where `U` is an [`ArraySize`] provided by [`typenum`]
//!   whose associated [`ArraySize::ArrayType<T>`] determines the inner array size.
//! - [`ArrayN<T, N>`]: type alias for [`Array`] which is const generic around `const N: usize`.
//!   This provides a linkage between const generics and [`typenum`].
//!
//! The [`Array`] type has an inner `pub [T; N]` field, which means writing a literal can be
//! expressed as follows:
//!
//! ```
//! use hybrid_array::{Array, sizes::U4};
//!
//! let arr: Array<u8, U4> = Array([1, 2, 3, 4]);
//! ```
//!
//! ### About [`typenum`]
//!
//! The [`typenum`] crate provides a type-level implementation of numbers and arithmetic operations.
//!
//! While [`typenum`] can be used to express arbitrary integers using the type system, the
//! `hybrid-array` crate is limited to the array sizes in the [`sizes`] module, which have
//! names like [`U0`][`sizes::U0`], [`U1`][`sizes::U1`], [`U2`][`sizes::U2`], [`U3`][`sizes::U3`],
//! etc. All supported sizes will have an impl of [`ArraySize`], which is the trait providing
//! linkage between [`typenum`]-based types and core arrays / const generics.
//!
//! [`ArraySize`] bounds on the [`typenum::Unsigned`] trait, which can be used to obtain integer
//! sizes of arrays via associated constants. For example, to obtain the size of an `ArraySize` as
//! a `usize`, use the associated [`typenum::Unsigned::USIZE`] constant.
//!
//! ## Relationship with `generic-array`
//!
//! `hybrid-array` is directly inspired by the [`generic-array`] crate.
//!
//! However, where `generic-array` predates const generics and uses a core which is built
//! on `unsafe` code, `hybrid-array`'s core implementation is built on safe code and const
//! generic implementations. This allows the inner `[T; N]` field of an `Array` to be `pub` as
//! noted above, and in general for the implementation to be significantly simpler and
//! easier-to-audit.
//!
//! The only places `hybrid-array` uses unsafe are where it is absolutely necessary, primarily
//! for reference conversions between `Array<T, U>` and `[T; N]`, and also to provide features
//! which are not yet stable in `core`/`std`, such as [`Array::try_from_fn`].
//!
//! [`generic-array`]: https://docs.rs/generic-array
//!
//! ## Migrating from `generic-array`
//!
//! *NOTE: this guide assumes a migration from `generic-array` v0.14*
//!
//! `hybrid-array` has been designed to largely be a drop-in replacement for
//! `generic-array`, albeit with a public inner array type and significantly less
//! `unsafe` code.
//!
//! The bulk of the migration work can be accomplished by making the following find/replace-style
//! substitutions in your `.rs` files:
//!
//! - Replace `generic_array` with `hybrid_array`
//! - Replace `GenericArray<T, U>` with `Array<T, U>`
//! - Replace `ArrayLength<T>` with `ArraySize`
//! - Replace usages of the `Concat` and `Split` traits with [`Array::concat`] and [`Array::split`]
//! - Replace `<U as ArrayLength<T>>::ArrayType` with `<U as ArraySize>::ArrayType<T>`
//! - Replace usages of the `arr![N; A, B, C]` macro with `Array([A, B, C])`
//!
//! If you have any questions, please
//! [start a discussion](https://github.com/RustCrypto/hybrid-array/discussions).

pub mod sizes;

mod from_fn;
mod iter;
mod traits;

#[cfg(feature = "serde")]
mod serde;

pub use crate::{iter::TryFromIteratorError, traits::*};
pub use typenum;

use core::{
    array::TryFromSliceError,
    borrow::{Borrow, BorrowMut},
    cmp::Ordering,
    fmt::{self, Debug},
    hash::{Hash, Hasher},
    mem::{self, ManuallyDrop, MaybeUninit},
    ops::{Add, Deref, DerefMut, Index, IndexMut, Sub},
    ptr,
    slice::{self, Iter, IterMut},
};
use typenum::{Diff, Sum};

#[cfg(feature = "bytemuck")]
use bytemuck::{Pod, Zeroable};

#[cfg(feature = "zeroize")]
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Type alias for [`Array`] which is const generic around a size `N`, ala `[T; N]`.
pub type ArrayN<T, const N: usize> = Array<T, <[T; N] as AssocArraySize>::Size>;

/// [`Array`] is a newtype for an inner `[T; N]` array where `N` is determined by a generic
/// [`ArraySize`] parameter, which is a marker trait for a numeric value determined by ZSTs that
/// impl the [`typenum::Unsigned`] trait.
///
/// The inner `[T; N]` field is `pub` which means it's possible to write [`Array`] literals like:
///
/// [`Array`] is defined as `repr(transparent)`, meaning it can be used anywhere an appropriately
/// sized `[T; N]` type is used in unsafe code / FFI.
///
/// ```
/// use hybrid_array::{Array, sizes::U3};
///
/// let arr: Array<u8, U3> = Array([1, 2, 3]);
/// ```
///
/// ## [`AsRef`] impls
///
/// The [`AsRef`] trait can be used to convert from `&Array<T, U>` to `&[T; N]` and vice versa:
///
/// ```
/// use hybrid_array::{Array, ArraySize, AssocArraySize, ArrayN, sizes::U3};
///
/// pub fn get_third_item_hybrid_array<T, U: ArraySize>(arr_ref: &Array<T, U>) -> &T {
///     &arr_ref[2]
/// }
///
/// pub fn get_third_item_const_generic<T, const N: usize>(arr_ref: &[T; N]) -> &T
/// where
///     [T; N]: AssocArraySize + AsRef<ArrayN<T, N>>
/// {
///     get_third_item_hybrid_array(arr_ref.as_ref())
/// }
///
/// assert_eq!(get_third_item_const_generic(&[1u8, 2, 3, 4]), &3);
/// ```
///
/// Note that the [`AssocArraySize`] trait can be used to determine the appropriate
/// [`Array`] size for a given `[T; N]`, and the [`ArrayN`] trait (which internally uses
/// [`AssocArraySize`]) can be used to determine the specific [`Array`] type for a given
/// const generic size.
#[repr(transparent)]
pub struct Array<T, U: ArraySize>(pub U::ArrayType<T>);

type SplitResult<T, U, N> = (Array<T, N>, Array<T, Diff<U, N>>);
type SplitRefResult<'a, T, U, N> = (&'a Array<T, N>, &'a Array<T, Diff<U, N>>);
type SplitRefMutResult<'a, T, U, N> = (&'a mut Array<T, N>, &'a mut Array<T, Diff<U, N>>);

impl<T, U> Array<T, U>
where
    U: ArraySize,
{
    /// Returns a slice containing the entire array. Equivalent to `&s[..]`.
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        self.0.as_ref()
    }

    /// Returns a mutable slice containing the entire array. Equivalent to `&mut s[..]`.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        self.0.as_mut()
    }

    /// Returns an iterator over the array.
    #[inline]
    pub fn iter(&self) -> Iter<'_, T> {
        self.as_slice().iter()
    }

    /// Returns an iterator that allows modifying each value.
    #[inline]
    pub fn iter_mut(&mut self) -> IterMut<'_, T> {
        self.as_mut().iter_mut()
    }

    /// Returns an array of the same size as `self`, with function `f` applied to each element in
    /// order.
    pub fn map<F, O>(self, f: F) -> Array<O, U>
    where
        F: FnMut(T) -> O,
    {
        self.into_iter().map(f).collect()
    }

    /// Concatenates `self` with `other`.
    #[inline]
    pub fn concat<N>(self, other: Array<T, N>) -> Array<T, Sum<U, N>>
    where
        N: ArraySize,
        U: Add<N>,
        Sum<U, N>: ArraySize,
    {
        self.into_iter().chain(other).collect()
    }

    /// Splits `self` at index `N` in two arrays.
    ///
    /// New arrays hold the original memory from `self`.
    #[inline]
    pub fn split<N>(self) -> SplitResult<T, U, N>
    where
        U: Sub<N>,
        N: ArraySize,
        Diff<U, N>: ArraySize,
    {
        unsafe {
            let array = ManuallyDrop::new(self);
            let head = ptr::read(array.as_ptr().cast());
            let tail = ptr::read(array.as_ptr().add(N::USIZE).cast());
            (head, tail)
        }
    }

    /// Splits `&self` at index `N` in two array references.
    #[inline]
    pub fn split_ref<N>(&self) -> SplitRefResult<'_, T, U, N>
    where
        U: Sub<N>,
        N: ArraySize,
        Diff<U, N>: ArraySize,
    {
        unsafe {
            let array_ptr = self.as_ptr();
            let head = &*array_ptr.cast();
            let tail = &*array_ptr.add(N::USIZE).cast();
            (head, tail)
        }
    }

    /// Splits `&mut self` at index `N` in two mutable array references.
    #[inline]
    pub fn split_ref_mut<N>(&mut self) -> SplitRefMutResult<'_, T, U, N>
    where
        U: Sub<N>,
        N: ArraySize,
        Diff<U, N>: ArraySize,
    {
        unsafe {
            let array_ptr = self.as_mut_ptr();
            let head = &mut *array_ptr.cast();
            let tail = &mut *array_ptr.add(N::USIZE).cast();
            (head, tail)
        }
    }

    /// Splits the shared slice into a slice of `U`-element arrays, starting at the beginning
    /// of the slice, and a remainder slice with length strictly less than `U`.
    ///
    /// # Panics
    /// Panics if `U` is 0.
    #[allow(clippy::arithmetic_side_effects)]
    #[inline]
    pub fn slice_as_chunks(buf: &[T]) -> (&[Self], &[T]) {
        assert_ne!(U::USIZE, 0, "chunk size must be non-zero");
        // Arithmetic safety: we have checked that `N::USIZE` is not zero, thus
        // division always returns correct result. `tail_pos` can not be bigger than `buf.len()`,
        // thus overflow on multiplication and underflow on substraction are impossible.
        let chunks_len = buf.len() / U::USIZE;
        let tail_pos = U::USIZE * chunks_len;
        let tail_len = buf.len() - tail_pos;
        unsafe {
            let ptr = buf.as_ptr();
            let chunks = slice::from_raw_parts(ptr.cast(), chunks_len);
            let tail = slice::from_raw_parts(ptr.add(tail_pos), tail_len);
            (chunks, tail)
        }
    }

    /// Splits the exclusive slice into a slice of `U`-element arrays, starting at the beginning
    /// of the slice, and a remainder slice with length strictly less than `U`.
    ///
    /// # Panics
    /// Panics if `U` is 0.
    #[allow(clippy::arithmetic_side_effects)]
    #[inline]
    pub fn slice_as_chunks_mut(buf: &mut [T]) -> (&mut [Self], &mut [T]) {
        assert_ne!(U::USIZE, 0, "chunk size must be non-zero");
        // Arithmetic safety: we have checked that `N::USIZE` is not zero, thus
        // division always returns correct result. `tail_pos` can not be bigger than `buf.len()`,
        // thus overflow on multiplication and underflow on substraction are impossible.
        let chunks_len = buf.len() / U::USIZE;
        let tail_pos = U::USIZE * chunks_len;
        let tail_len = buf.len() - tail_pos;
        unsafe {
            let ptr = buf.as_mut_ptr();
            let chunks = slice::from_raw_parts_mut(ptr.cast(), chunks_len);
            let tail = slice::from_raw_parts_mut(ptr.add(tail_pos), tail_len);
            (chunks, tail)
        }
    }

    /// Convert the given slice into a reference to a hybrid array.
    ///
    /// # Panics
    ///
    /// Panics if the slice's length doesn't match the array type.
    #[deprecated(since = "0.2.0", note = "use `TryFrom` instead")]
    #[inline]
    pub fn from_slice(slice: &[T]) -> &Self {
        slice.try_into().expect("slice length mismatch")
    }

    /// Convert the given mutable slice to a mutable reference to a hybrid array.
    ///
    /// # Panics
    ///
    /// Panics if the slice's length doesn't match the array type.
    #[deprecated(since = "0.2.0", note = "use `TryFrom` instead")]
    #[inline]
    pub fn from_mut_slice(slice: &mut [T]) -> &mut Self {
        slice.try_into().expect("slice length mismatch")
    }

    /// Clone the contents of the slice as a new hybrid array.
    ///
    /// # Panics
    ///
    /// Panics if the slice's length doesn't match the array type.
    #[deprecated(since = "0.2.0", note = "use `TryFrom` instead")]
    #[inline]
    pub fn clone_from_slice(slice: &[T]) -> Self
    where
        Self: Clone,
    {
        slice.try_into().expect("slice length mismatch")
    }
}

// Impls which depend on the inner array type being `[T; N]`.
impl<T, U, const N: usize> Array<T, U>
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    /// Transform slice to slice of core array type.
    #[inline]
    pub fn cast_slice_to_core(slice: &[Self]) -> &[[T; N]] {
        // SAFETY: `Self` is a `repr(transparent)` newtype for `[T; N]`
        unsafe { slice::from_raw_parts(slice.as_ptr().cast(), slice.len()) }
    }

    /// Transform mutable slice to mutable slice of core array type.
    #[inline]
    pub fn cast_slice_to_core_mut(slice: &mut [Self]) -> &mut [[T; N]] {
        // SAFETY: `Self` is a `repr(transparent)` newtype for `[T; N]`
        unsafe { slice::from_raw_parts_mut(slice.as_mut_ptr().cast(), slice.len()) }
    }

    /// Transform slice to slice of core array type.
    #[inline]
    pub fn cast_slice_from_core(slice: &[[T; N]]) -> &[Self] {
        // SAFETY: `Self` is a `repr(transparent)` newtype for `[T; N]`
        unsafe { slice::from_raw_parts(slice.as_ptr().cast(), slice.len()) }
    }

    /// Transform mutable slice to mutable slice of core array type.
    #[inline]
    pub fn cast_slice_from_core_mut(slice: &mut [[T; N]]) -> &mut [Self] {
        // SAFETY: `Self` is a `repr(transparent)` newtype for `[T; N]`
        unsafe { slice::from_raw_parts_mut(slice.as_mut_ptr().cast(), slice.len()) }
    }
}

impl<T, U> Array<MaybeUninit<T>, U>
where
    U: ArraySize,
{
    /// Create an uninitialized array of [`MaybeUninit`]s for the given type.
    pub const fn uninit() -> Array<MaybeUninit<T>, U> {
        // SAFETY: `Array` is a `repr(transparent)` newtype for `[MaybeUninit<T>, N]`, i.e. an
        // array of uninitialized memory mediated via the `MaybeUninit` interface, where the inner
        // type is constrained by `ArraySize` impls which can only be added by this crate.
        //
        // Calling `uninit().assume_init()` triggers the `clippy::uninit_assumed_init` lint, but
        // as just mentioned the inner type we're "assuming init" for is `[MaybeUninit<T>, N]`,
        // i.e. an array of uninitialized memory, which is always valid because definitionally no
        // initialization is required of uninitialized memory.
        #[allow(clippy::uninit_assumed_init)]
        Self(unsafe { MaybeUninit::uninit().assume_init() })
    }

    /// Extract the values from an array of `MaybeUninit` containers.
    ///
    /// # Safety
    ///
    /// It is up to the caller to guarantee that all elements of the array are in an initialized
    /// state.
    #[inline]
    pub unsafe fn assume_init(self) -> Array<T, U> {
        // `Array` is a `repr(transparent)` newtype for a generic inner type which is constrained to
        // be `[T; N]` by the `ArraySize` impls in this crate.
        //
        // Since we're working with a type-erased inner type and ultimately trying to convert
        // `[MaybeUninit<T>; N]` to `[T; N]`, we can't use simpler approaches like a pointer cast
        // or `transmute`, since the compiler can't prove to itself that the size will be the same.
        //
        // We've taken unique ownership of `self`, which is a `MaybeUninit` array, and as such we
        // don't need to worry about `Drop` impls because `MaybeUninit` does not impl `Drop`.
        // Since we have unique ownership of `self`, it's okay to make a copy because we're throwing
        // the original away (and this should all get optimized to a noop by the compiler, anyway).
        mem::transmute_copy(&self)
    }
}

impl<T, U> AsRef<Array<T, U>> for Array<T, U>
where
    U: ArraySize,
{
    #[inline]
    fn as_ref(&self) -> &Self {
        self
    }
}

impl<T, U> AsRef<[T]> for Array<T, U>
where
    U: ArraySize,
{
    #[inline]
    fn as_ref(&self) -> &[T] {
        self.0.as_ref()
    }
}

impl<T, U, const N: usize> AsRef<[T; N]> for Array<T, U>
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn as_ref(&self) -> &[T; N] {
        &self.0
    }
}

impl<T, U, const N: usize> AsRef<Array<T, U>> for [T; N]
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn as_ref(&self) -> &Array<T, U> {
        // SAFETY: `Self` is a `repr(transparent)` newtype for `[T; $len]`
        unsafe { &*self.as_ptr().cast() }
    }
}

impl<T, U> AsMut<[T]> for Array<T, U>
where
    U: ArraySize,
{
    #[inline]
    fn as_mut(&mut self) -> &mut [T] {
        self.0.as_mut()
    }
}

impl<T, U, const N: usize> AsMut<[T; N]> for Array<T, U>
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn as_mut(&mut self) -> &mut [T; N] {
        &mut self.0
    }
}

impl<T, U, const N: usize> AsMut<Array<T, U>> for [T; N]
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn as_mut(&mut self) -> &mut Array<T, U> {
        // SAFETY: `Self` is a `repr(transparent)` newtype for `[T; $len]`
        unsafe { &mut *self.as_mut_ptr().cast() }
    }
}

impl<T, U> Borrow<[T]> for Array<T, U>
where
    U: ArraySize,
{
    #[inline]
    fn borrow(&self) -> &[T] {
        self.0.as_ref()
    }
}

impl<T, U, const N: usize> Borrow<[T; N]> for Array<T, U>
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn borrow(&self) -> &[T; N] {
        &self.0
    }
}

impl<T, U> BorrowMut<[T]> for Array<T, U>
where
    U: ArraySize,
{
    #[inline]
    fn borrow_mut(&mut self) -> &mut [T] {
        self.0.as_mut()
    }
}

impl<T, U, const N: usize> BorrowMut<[T; N]> for Array<T, U>
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn borrow_mut(&mut self) -> &mut [T; N] {
        &mut self.0
    }
}

impl<T, U> Clone for Array<T, U>
where
    T: Clone,
    U: ArraySize,
{
    #[inline]
    fn clone(&self) -> Self {
        Self::from_fn(|n| self.0.as_ref()[n].clone())
    }
}

impl<T, U> Copy for Array<T, U>
where
    T: Copy,
    U: ArraySize,
    U::ArrayType<T>: Copy,
{
}

impl<T, U> Debug for Array<T, U>
where
    T: Debug,
    U: ArraySize,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Array").field(&self.0.as_ref()).finish()
    }
}

impl<T, U> Default for Array<T, U>
where
    T: Default,
    U: ArraySize,
{
    #[inline]
    fn default() -> Self {
        Self::from_fn(|_| Default::default())
    }
}

impl<T, U> Deref for Array<T, U>
where
    U: ArraySize,
{
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        self.0.as_ref()
    }
}

impl<T, U> DerefMut for Array<T, U>
where
    U: ArraySize,
{
    #[inline]
    fn deref_mut(&mut self) -> &mut [T] {
        self.0.as_mut()
    }
}

impl<T, U> Eq for Array<T, U>
where
    T: Eq,
    U: ArraySize,
{
}

impl<T, U, const N: usize> From<[T; N]> for Array<T, U>
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn from(arr: [T; N]) -> Array<T, U> {
        Array(arr)
    }
}

impl<T, U, const N: usize> From<Array<T, U>> for [T; N]
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn from(arr: Array<T, U>) -> [T; N] {
        arr.0
    }
}

impl<'a, T, U, const N: usize> From<&'a [T; N]> for &'a Array<T, U>
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn from(array_ref: &'a [T; N]) -> &'a Array<T, U> {
        array_ref.as_ref()
    }
}

impl<'a, T, U, const N: usize> From<&'a Array<T, U>> for &'a [T; N]
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn from(array_ref: &'a Array<T, U>) -> &'a [T; N] {
        array_ref.as_ref()
    }
}

impl<'a, T, U, const N: usize> From<&'a mut [T; N]> for &'a mut Array<T, U>
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn from(array_ref: &'a mut [T; N]) -> &'a mut Array<T, U> {
        array_ref.as_mut()
    }
}

impl<'a, T, U, const N: usize> From<&'a mut Array<T, U>> for &'a mut [T; N]
where
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn from(array_ref: &'a mut Array<T, U>) -> &'a mut [T; N] {
        array_ref.as_mut()
    }
}

impl<T, U> Hash for Array<T, U>
where
    T: Hash,
    U: ArraySize,
{
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.as_ref().hash(state);
    }
}

impl<T, I, U> Index<I> for Array<T, U>
where
    [T]: Index<I>,
    U: ArraySize,
{
    type Output = <[T] as Index<I>>::Output;

    #[inline]
    fn index(&self, index: I) -> &Self::Output {
        Index::index(self.as_slice(), index)
    }
}

impl<T, I, U> IndexMut<I> for Array<T, U>
where
    [T]: IndexMut<I>,
    U: ArraySize,
{
    #[inline]
    fn index_mut(&mut self, index: I) -> &mut Self::Output {
        IndexMut::index_mut(self.as_mut_slice(), index)
    }
}

impl<T, U> PartialEq for Array<T, U>
where
    T: PartialEq,
    U: ArraySize,
{
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.0.as_ref().eq(other.0.as_ref())
    }
}

impl<T, U, const N: usize> PartialEq<[T; N]> for Array<T, U>
where
    T: PartialEq,
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn eq(&self, other: &[T; N]) -> bool {
        self.0.eq(other)
    }
}

impl<T, U, const N: usize> PartialEq<Array<T, U>> for [T; N]
where
    T: PartialEq,
    U: ArraySize<ArrayType<T> = [T; N]>,
{
    #[inline]
    fn eq(&self, other: &Array<T, U>) -> bool {
        self.eq(&other.0)
    }
}

impl<T, U> PartialOrd for Array<T, U>
where
    T: PartialOrd,
    U: ArraySize,
{
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.0.as_ref().partial_cmp(other.0.as_ref())
    }
}

impl<T, U> Ord for Array<T, U>
where
    T: Ord,
    U: ArraySize,
{
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.as_ref().cmp(other.0.as_ref())
    }
}

/// SAFETY: `Array` is a `repr(transparent)` newtype for `[T; N]`, so as long as `T: Send` it should
/// also be `Send`.
unsafe impl<T, U: ArraySize> Send for Array<T, U> where T: Send {}

/// SAFETY: `Array` is a `repr(transparent)` newtype for `[T; N]`, so as long as `T: Sync` it should
/// also be `Sync`.
unsafe impl<T, U: ArraySize> Sync for Array<T, U> where T: Sync {}

impl<'a, T, U> TryFrom<&'a [T]> for Array<T, U>
where
    Self: Clone,
    U: ArraySize,
{
    type Error = TryFromSliceError;

    #[inline]
    fn try_from(slice: &'a [T]) -> Result<Array<T, U>, TryFromSliceError> {
        <&'a Self>::try_from(slice).cloned()
    }
}

impl<'a, T, U> TryFrom<&'a [T]> for &'a Array<T, U>
where
    U: ArraySize,
{
    type Error = TryFromSliceError;

    #[inline]
    fn try_from(slice: &'a [T]) -> Result<Self, TryFromSliceError> {
        check_slice_length::<T, U>(slice)?;

        // SAFETY: `Array<T, U>` is a `repr(transparent)` newtype for a core
        // array with length checked above.
        Ok(unsafe { &*slice.as_ptr().cast() })
    }
}

impl<'a, T, U> TryFrom<&'a mut [T]> for &'a mut Array<T, U>
where
    U: ArraySize,
{
    type Error = TryFromSliceError;

    #[inline]
    fn try_from(slice: &'a mut [T]) -> Result<Self, TryFromSliceError> {
        check_slice_length::<T, U>(slice)?;

        // SAFETY: `Array<T, U>` is a `repr(transparent)` newtype for a core
        // array with length checked above.
        Ok(unsafe { &mut *slice.as_mut_ptr().cast() })
    }
}

#[cfg(feature = "bytemuck")]
unsafe impl<T, U> Pod for Array<T, U>
where
    T: Pod,
    U: ArraySize,
    U::ArrayType<T>: Copy,
{
}

#[cfg(feature = "bytemuck")]
unsafe impl<T, U> Zeroable for Array<T, U>
where
    T: Zeroable,
    U: ArraySize,
{
}

#[cfg(feature = "zeroize")]
impl<T, U> Zeroize for Array<T, U>
where
    T: Zeroize,
    U: ArraySize,
{
    #[inline]
    fn zeroize(&mut self) {
        self.0.as_mut().iter_mut().zeroize()
    }
}

#[cfg(feature = "zeroize")]
impl<T, U> ZeroizeOnDrop for Array<T, U>
where
    T: ZeroizeOnDrop,
    U: ArraySize,
{
}

/// Generate a [`TryFromSliceError`] if the slice doesn't match the given length.
#[cfg_attr(debug_assertions, allow(clippy::panic_in_result_fn))]
fn check_slice_length<T, U: ArraySize>(slice: &[T]) -> Result<(), TryFromSliceError> {
    debug_assert_eq!(Array::<(), U>::default().len(), U::USIZE);

    if slice.len() != U::USIZE {
        // Hack: `TryFromSliceError` lacks a public constructor
        <&[T; 1]>::try_from([].as_slice())?;

        #[cfg(debug_assertions)]
        unreachable!();
    }

    Ok(())
}
