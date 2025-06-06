//! Support for constructing arrays using a provided iterator function and other iterator-related
//! functionality.

use crate::{Array, ArraySize};
use core::{
    fmt,
    mem::size_of,
    slice::{Iter, IterMut},
};

/// Couldn't construct an array from an iterator because the number of items in the iterator
/// didn't match the array size.
#[derive(Clone, Copy, Debug)]
pub struct TryFromIteratorError;

impl fmt::Display for TryFromIteratorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("iterator did not contain the correct number of items for the array size")
    }
}

impl core::error::Error for TryFromIteratorError {}

impl<T, U> Array<T, U>
where
    U: ArraySize,
{
    /// Construct an array from the given iterator, returning [`TryFromIteratorError`] in the event
    /// that the number of items in the iterator does not match the array size.
    ///
    /// # Errors
    ///
    /// Returns [`TryFromIteratorError`] in the event the iterator does not return a number of
    /// items which is exactly equal to the array size.
    pub fn try_from_iter<I: IntoIterator<Item = T>>(iter: I) -> Result<Self, TryFromIteratorError> {
        let mut iter = iter.into_iter();
        let ret = Self::try_from_fn(|_| iter.next().ok_or(TryFromIteratorError))?;

        match iter.next() {
            None => Ok(ret),
            Some(_) => Err(TryFromIteratorError),
        }
    }
}

impl<T, U> FromIterator<T> for Array<T, U>
where
    U: ArraySize,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut iter = iter.into_iter();
        let ret = Self::from_fn(|_| {
            iter.next()
                .expect("iterator should have enough items to fill array")
        });

        assert!(
            iter.next().is_none(),
            "too many items in iterator to fit in array"
        );

        ret
    }
}

impl<T, U> IntoIterator for Array<T, U>
where
    U: ArraySize,
{
    type Item = T;
    type IntoIter = <U::ArrayType<T> as IntoIterator>::IntoIter;

    /// Creates a consuming iterator, that is, one that moves each value out of the array (from
    /// start to end).
    ///
    /// The array cannot be used after calling this unless `T` implements `Copy`, so the whole
    /// array is copied.
    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, T, U> IntoIterator for &'a Array<T, U>
where
    U: ArraySize,
{
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    #[inline]
    fn into_iter(self) -> Iter<'a, T> {
        self.iter()
    }
}

impl<'a, T, U> IntoIterator for &'a mut Array<T, U>
where
    U: ArraySize,
{
    type Item = &'a mut T;
    type IntoIter = IterMut<'a, T>;

    #[inline]
    fn into_iter(self) -> IterMut<'a, T> {
        self.iter_mut()
    }
}

impl<T, U, V> Array<Array<T, U>, V>
where
    U: ArraySize,
    V: ArraySize,
{
    /// Takes a `&mut Array<Array<T, N>,M>`, and flattens it to a `&mut [T]`.
    ///
    /// # Panics
    ///
    /// This panics if the length of the resulting slice would overflow a `usize`.
    ///
    /// This is only possible when flattening a slice of arrays of zero-sized
    /// types, and thus tends to be irrelevant in practice. If
    /// `size_of::<T>() > 0`, this will never panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use hybrid_array::{Array, typenum::U3};
    ///
    /// fn add_5_to_all(slice: &mut [i32]) {
    ///     for i in slice {
    ///         *i += 5;
    ///     }
    /// }
    ///
    /// let mut array: Array<Array<i32, U3>, U3> = Array([Array([1_i32, 2, 3]), Array([4, 5, 6]), Array([7, 8, 9])]);
    /// add_5_to_all(array.as_flattened_mut());
    /// assert_eq!(array, Array([Array([6, 7, 8]), Array([9, 10, 11]), Array([12, 13, 14])]));
    /// ```
    pub fn as_flattened_mut(&mut self) -> &mut [T] {
        let len = if size_of::<T>() == 0 {
            self.len()
                .checked_mul(U::USIZE)
                .expect("slice len overflow")
        } else {
            // SAFETY: `self.len() * N` cannot overflow because `self` is
            // already in the address space.
            unsafe { self.len().unchecked_mul(U::USIZE) }
        };
        // SAFETY: `[T]` is layout-identical to `[T; U]`
        unsafe { core::slice::from_raw_parts_mut(self.as_mut_ptr().cast(), len) }
    }

    /// Takes a `&Array<Array<T, N>, >>`, and flattens it to a `&[T]`.
    ///
    /// # Panics
    ///
    /// This panics if the length of the resulting slice would overflow a `usize`.
    ///
    /// This is only possible when flattening a slice of arrays of zero-sized
    /// types, and thus tends to be irrelevant in practice. If
    /// `size_of::<T>() > 0`, this will never panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use hybrid_array::{Array, typenum::{U0, U2, U3, U5, U10}};
    ///
    /// let a: Array<Array<usize, U3>, U2> = Array([Array([1, 2, 3]), Array([4, 5, 6])]);
    /// assert_eq!(a.as_flattened(), &[1, 2, 3, 4, 5, 6]);
    ///
    /// let b: Array<Array<usize, U2>, U3> = Array([Array([1, 2]), Array([3, 4]), Array([5, 6])]);
    /// assert_eq!(a.as_flattened(), b.as_flattened());
    ///
    /// let c: Array<[usize; 2], U3> = Array([[1, 2], [3, 4], [5, 6]]);
    /// assert_eq!(a.as_flattened(), c.as_flattened());
    ///
    /// let slice_of_empty_arrays: &Array<Array<i32, U5>, U0> = &Array::from_fn(|_| Array([1, 2, 3, 4, 5]));
    /// assert!(slice_of_empty_arrays.as_flattened().is_empty());
    ///
    /// let empty_slice_of_arrays: &Array<Array<u32, U10>, U0>  = &Array([]);
    /// assert!(empty_slice_of_arrays.as_flattened().is_empty());
    /// ```
    pub fn as_flattened(&self) -> &[T] {
        let len = if size_of::<T>() == 0 {
            self.len()
                .checked_mul(U::USIZE)
                .expect("slice len overflow")
        } else {
            // SAFETY: `self.len() * N` cannot overflow because `self` is
            // already in the address space.
            unsafe { self.len().unchecked_mul(U::USIZE) }
        };
        // SAFETY: `[T]` is layout-identical to `[T; U]`
        unsafe { core::slice::from_raw_parts(self.as_ptr().cast(), len) }
    }
}
