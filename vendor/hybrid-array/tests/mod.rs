use hybrid_array::{Array, ArrayN};
use std::mem::MaybeUninit;
use typenum::{U0, U2, U3, U4, U5, U6, U7};

const EXAMPLE_SLICE: &[u8] = &[1, 2, 3, 4, 5, 6];

/// Ensure `ArrayN` works as expected.
const _FOO: ArrayN<u8, 4> = Array([1, 2, 3, 4]);

#[test]
fn tryfrom_slice_for_array() {
    assert!(Array::<u8, U0>::try_from(EXAMPLE_SLICE).is_err());
    assert!(Array::<u8, U3>::try_from(EXAMPLE_SLICE).is_err());

    let array_ref = Array::<u8, U6>::try_from(EXAMPLE_SLICE).expect("slice contains 6 bytes");
    assert_eq!(&*array_ref, EXAMPLE_SLICE);

    assert!(Array::<u8, U7>::try_from(EXAMPLE_SLICE).is_err());
}

#[test]
fn tryfrom_slice_for_array_ref() {
    assert!(<&Array<u8, U0>>::try_from(EXAMPLE_SLICE).is_err());
    assert!(<&Array::<u8, U3>>::try_from(EXAMPLE_SLICE).is_err());

    let array_ref = <&Array<u8, U6>>::try_from(EXAMPLE_SLICE).expect("slice contains 6 bytes");
    assert_eq!(array_ref.as_slice(), EXAMPLE_SLICE);

    assert!(<&Array::<u8, U7>>::try_from(EXAMPLE_SLICE).is_err());
}

#[test]
fn concat() {
    let prefix = Array::<u8, U2>::try_from(&EXAMPLE_SLICE[..2]).unwrap();
    let suffix = Array::<u8, U4>::try_from(&EXAMPLE_SLICE[2..]).unwrap();

    let array = prefix.concat(suffix);
    assert_eq!(array.as_slice(), EXAMPLE_SLICE);
}

#[test]
fn split() {
    let array = Array::<u8, U6>::try_from(EXAMPLE_SLICE).unwrap();
    let (prefix, suffix) = array.split::<U2>();

    assert_eq!(prefix.as_slice(), &EXAMPLE_SLICE[..2]);
    assert_eq!(suffix.as_slice(), &EXAMPLE_SLICE[2..]);
}

#[test]
fn split_ref() {
    let array = Array::<u8, U6>::try_from(EXAMPLE_SLICE).unwrap();
    let (prefix, suffix) = array.split_ref::<U3>();

    assert_eq!(prefix.as_slice(), &EXAMPLE_SLICE[..3]);
    assert_eq!(suffix.as_slice(), &EXAMPLE_SLICE[3..]);
}

#[test]
fn split_ref_mut() {
    let array = &mut Array::<u8, U6>::try_from(EXAMPLE_SLICE).unwrap();
    let (prefix, suffix) = array.split_ref_mut::<U4>();

    assert_eq!(prefix.as_slice(), &EXAMPLE_SLICE[..4]);
    assert_eq!(suffix.as_slice(), &EXAMPLE_SLICE[4..]);
}

#[test]
fn from_fn() {
    let array = Array::<u8, U6>::from_fn(|n| (n + 1) as u8);
    assert_eq!(array.as_slice(), EXAMPLE_SLICE);
}

#[test]
fn try_from_fn() {
    let array = Array::<u8, U6>::try_from_fn::<()>(|n| Ok((n + 1) as u8)).unwrap();
    assert_eq!(array.as_slice(), EXAMPLE_SLICE);

    let err = Array::<u8, U6>::try_from_fn::<&'static str>(|_| Err("err"))
        .err()
        .unwrap();

    assert_eq!(err, "err");
}

#[test]
fn from_iterator_correct_size() {
    let array: Array<u8, U6> = EXAMPLE_SLICE.iter().copied().collect();
    assert_eq!(array.as_slice(), EXAMPLE_SLICE);
}

#[test]
#[should_panic]
fn from_iterator_too_short() {
    let _array: Array<u8, U7> = EXAMPLE_SLICE.iter().copied().collect();
}

#[test]
#[should_panic]
fn from_iterator_too_long() {
    let _array: Array<u8, U5> = EXAMPLE_SLICE.iter().copied().collect();
}

#[test]
fn try_from_iterator_correct_size() {
    let array = Array::<u8, U6>::try_from_iter(EXAMPLE_SLICE.iter().copied()).unwrap();
    assert_eq!(array.as_slice(), EXAMPLE_SLICE);
}

#[test]
fn try_from_iterator_too_short() {
    let result = Array::<u8, U7>::try_from_iter(EXAMPLE_SLICE.iter().copied());
    assert!(result.is_err());
}

#[test]
fn try_from_iterator_too_long() {
    let result = Array::<u8, U5>::try_from_iter(EXAMPLE_SLICE.iter().copied());
    assert!(result.is_err());
}

#[test]
fn maybe_uninit() {
    let mut uninit_array = Array::<MaybeUninit<u8>, U6>::uninit();

    for i in 0..6 {
        uninit_array[i].write(EXAMPLE_SLICE[i]);
    }

    let array = unsafe { uninit_array.assume_init() };
    assert_eq!(array.as_slice(), EXAMPLE_SLICE);
}

#[test]
fn map() {
    let base = Array::<u8, U4>::from([1, 2, 3, 4]);
    let expected = Array::<u16, U4>::from([2, 3, 4, 5]);
    assert_eq!(base.map(|item| (item as u16) + 1), expected);
}

#[test]
#[allow(deprecated)]
fn clone_from_slice() {
    let array = Array::<u8, U6>::clone_from_slice(EXAMPLE_SLICE);
    assert_eq!(array.as_slice(), EXAMPLE_SLICE);
}
