//! This is an internal module, no stability guarantees are provided. Use at
//! your own risk.

#![doc(hidden)]

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::{mem::MaybeUninit, ptr::NonNull};

use crate::{Clamped, JsError, JsObject, JsValue};
use cfg_if::cfg_if;

macro_rules! tys {
    ($($a:ident)*) => (tys! { @ ($($a)*) 0 });
    (@ () $v:expr) => {};
    (@ ($a:ident $($b:ident)*) $v:expr) => {
        pub const $a: u32 = $v;
        tys!(@ ($($b)*) $v+1);
    }
}

// NB: this list must be kept in sync with `crates/cli-support/src/descriptor.rs`
tys! {
    I8
    U8
    I16
    U16
    I32
    U32
    I64
    U64
    I128
    U128
    F32
    F64
    BOOLEAN
    FUNCTION
    CLOSURE
    CACHED_STRING
    STRING
    REF
    REFMUT
    LONGREF
    SLICE
    VECTOR
    EXTERNREF
    NAMED_EXTERNREF
    ENUM
    STRING_ENUM
    RUST_STRUCT
    CHAR
    OPTIONAL
    RESULT
    UNIT
    CLAMPED
    NONNULL
}

#[inline(always)] // see the wasm-interpreter crate
#[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
pub fn inform(a: u32) {
    unsafe { super::__wbindgen_describe(a) }
}

pub trait WasmDescribe {
    fn describe();
}

/// Trait for element types to implement WasmDescribe for vectors of
/// themselves.
pub trait WasmDescribeVector {
    fn describe_vector();
}

macro_rules! simple {
    ($($t:ident => $d:ident)*) => ($(
        impl WasmDescribe for $t {
            #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
            fn describe() { inform($d) }
        }
    )*)
}

simple! {
    i8 => I8
    u8 => U8
    i16 => I16
    u16 => U16
    i32 => I32
    u32 => U32
    i64 => I64
    u64 => U64
    i128 => I128
    u128 => U128
    isize => I32
    usize => U32
    f32 => F32
    f64 => F64
    bool => BOOLEAN
    char => CHAR
    JsValue => EXTERNREF
}

cfg_if! {
    if #[cfg(feature = "enable-interning")] {
        simple! {
            str => CACHED_STRING
        }

    } else {
        simple! {
            str => STRING
        }
    }
}

impl<T> WasmDescribe for *const T {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        inform(U32)
    }
}

impl<T> WasmDescribe for *mut T {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        inform(U32)
    }
}

impl<T> WasmDescribe for NonNull<T> {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        inform(NONNULL)
    }
}

impl<T: WasmDescribe> WasmDescribe for [T] {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        inform(SLICE);
        T::describe();
    }
}

impl<T: WasmDescribe + ?Sized> WasmDescribe for &T {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        inform(REF);
        T::describe();
    }
}

impl<T: WasmDescribe + ?Sized> WasmDescribe for &mut T {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        inform(REFMUT);
        T::describe();
    }
}

cfg_if! {
    if #[cfg(feature = "enable-interning")] {
        simple! {
            String => CACHED_STRING
        }

    } else {
        simple! {
            String => STRING
        }
    }
}

impl WasmDescribeVector for JsValue {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe_vector() {
        inform(VECTOR);
        JsValue::describe();
    }
}

impl<T: JsObject> WasmDescribeVector for T {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe_vector() {
        inform(VECTOR);
        T::describe();
    }
}

impl<T: WasmDescribeVector> WasmDescribe for Box<[T]> {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        T::describe_vector();
    }
}

impl<T> WasmDescribe for Vec<T>
where
    Box<[T]>: WasmDescribe,
{
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        <Box<[T]>>::describe();
    }
}

impl<T: WasmDescribe> WasmDescribe for Option<T> {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        inform(OPTIONAL);
        T::describe();
    }
}

impl WasmDescribe for () {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        inform(UNIT)
    }
}

impl<T: WasmDescribe, E: Into<JsValue>> WasmDescribe for Result<T, E> {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        inform(RESULT);
        T::describe();
    }
}

impl<T: WasmDescribe> WasmDescribe for MaybeUninit<T> {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        T::describe();
    }
}

impl<T: WasmDescribe> WasmDescribe for Clamped<T> {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        inform(CLAMPED);
        T::describe();
    }
}

impl WasmDescribe for JsError {
    #[cfg_attr(wasm_bindgen_unstable_test_coverage, coverage(off))]
    fn describe() {
        JsValue::describe();
    }
}
