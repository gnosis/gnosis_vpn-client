//! Trait definitions.

use crate::Array;
use core::{
    borrow::{Borrow, BorrowMut},
    ops::{Index, IndexMut, Range},
};
use typenum::Unsigned;

/// Trait which associates a [`usize`] size and `ArrayType` with a
/// `typenum`-provided [`Unsigned`] integer.
///
/// # Safety
///
/// `ArrayType` MUST be an array with a number of elements exactly equal to
/// [`Unsigned::USIZE`]. Breaking this requirement will cause undefined behavior.
///
/// NOTE: This trait is effectively sealed and can not be implemented by third-party crates.
/// It is implemented only for a number of types defined in [`typenum::consts`].
pub unsafe trait ArraySize: Unsigned {
    /// Array type which corresponds to this size.
    ///
    /// This is always defined to be `[T; N]` where `N` is the same as
    /// [`ArraySize::USIZE`][`typenum::Unsigned::USIZE`].
    type ArrayType<T>: AssocArraySize<Size = Self>
        + AsRef<[T]>
        + AsMut<[T]>
        + Borrow<[T]>
        + BorrowMut<[T]>
        + From<Array<T, Self>>
        + Index<usize>
        + Index<Range<usize>>
        + IndexMut<usize>
        + IndexMut<Range<usize>>
        + Into<Array<T, Self>>
        + IntoIterator<Item = T>;
}

/// Associates an [`ArraySize`] with a given type. Can be used to accept `[T; N]` const generic
/// arguments and convert to [`Array`] internally.
///
/// This trait is also the magic glue that makes the [`ArrayN`][`crate::ArrayN`] type alias work.
///
/// # Example
///
/// ```
/// use hybrid_array::{ArrayN, AssocArraySize};
///
/// pub fn example<const N: usize>(bytes: &[u8; N])
/// where
///     [u8; N]: AssocArraySize + AsRef<ArrayN<u8, N>>
/// {
///     // _arrayn is ArrayN<u8, N>
///     let _arrayn = bytes.as_ref();
/// }
/// ```
pub trait AssocArraySize: Sized {
    /// Size of an array type, expressed as a [`typenum`]-based [`ArraySize`].
    type Size: ArraySize;
}

impl<T, U> AssocArraySize for Array<T, U>
where
    U: ArraySize,
{
    type Size = U;
}
