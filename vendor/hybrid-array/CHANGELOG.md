# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.2.3 (2024-12-07)
### Added
- Additional ML-DSA sizes ([#108])

[#108]: https://github.com/RustCrypto/hybrid-array/pull/108

## 0.2.2 (2024-11-11)
### Added
- FrodoKEM sizes ([#104])

[#104]: https://github.com/RustCrypto/hybrid-array/pull/104

## 0.2.1 (2024-10-20)
### Fixed
- MSRV badge ([9d47c798](https://github.com/RustCrypto/hybrid-array/commit/9d47c79861057b3a04bb19cb2dfaa1f75cbf9ddd))

## 0.2.0 (2024-10-19)
### Added
- Reference conversion support from core arrays ([utils#904])
- Impl `Default` for `Array` ([utils#905])
- `Deref`/`DerefMut` impls for `Array` ([utils#908], [utils#913])
- Impl `From<Array<T, U>>` for `[T; N]` ([utils#945])
- Impl `IntoIterator` for all `ArraySize`s ([utils#956])
- Impl `IntoIterator` for references to all `ArraySize`s ([utils#957])
- Concat and split methods ([utils#958])
- `slice_as_chunks(_mut)` methods ([utils#974])
- Impl `Zeroize`/`ZeroizeOnDrop` for `Array` ([utils#984])
- `AssocArraySize` trait ([utils#1006], [#40])
- `sizes` submodule ([utils#1014], [#68])
- `ArrayN` type alias ([utils#1017])
- Impl `FromIterator` ([utils#1039])
- `Array::try_from_iter` ([#4])
- Helper functions for `Array<MaybeUninit<T>, U>` ([#8])
- `Send` and `Sync` impls for `Array` ([#15])
- `Array::map` ([#61])
- Support all array sizes up to `U512` ([#67])
- `Array<Array<...>>::as_flattened{_mut}()` ([#86])
- `serde` support ([#88])
- `bytemuck` support ([#99])

### Changed
- Use GATs for `ArraySize` ([utils#893])
- Make `ArraySize` an `unsafe trait` ([utils#914])
- MSRV 1.81 ([#85])

### Removed
- `ByteArray` type alias ([utils#995])
- `ArrayOps` trait ([#30])
- `std` feature ([#85])

[utils#893]: https://github.com/RustCrypto/utils/pull/893
[utils#904]: https://github.com/RustCrypto/utils/pull/904
[utils#905]: https://github.com/RustCrypto/utils/pull/905
[utils#908]: https://github.com/RustCrypto/utils/pull/908
[utils#913]: https://github.com/RustCrypto/utils/pull/913
[utils#914]: https://github.com/RustCrypto/utils/pull/914
[utils#945]: https://github.com/RustCrypto/utils/pull/945
[utils#956]: https://github.com/RustCrypto/utils/pull/956
[utils#957]: https://github.com/RustCrypto/utils/pull/957
[utils#958]: https://github.com/RustCrypto/utils/pull/958
[utils#974]: https://github.com/RustCrypto/utils/pull/974
[utils#984]: https://github.com/RustCrypto/utils/pull/984
[utils#995]: https://github.com/RustCrypto/utils/pull/995
[utils#1006]: https://github.com/RustCrypto/utils/pull/1006
[utils#1014]: https://github.com/RustCrypto/utils/pull/1014
[utils#1017]: https://github.com/RustCrypto/utils/pull/1017
[utils#1039]: https://github.com/RustCrypto/utils/pull/1039
[#4]: https://github.com/RustCrypto/hybrid-array/pull/4
[#8]: https://github.com/RustCrypto/hybrid-array/pull/8
[#15]: https://github.com/RustCrypto/hybrid-array/pull/15
[#30]: https://github.com/RustCrypto/hybrid-array/pull/30
[#40]: https://github.com/RustCrypto/hybrid-array/pull/40
[#61]: https://github.com/RustCrypto/hybrid-array/pull/61
[#67]: https://github.com/RustCrypto/hybrid-array/pull/67
[#68]: https://github.com/RustCrypto/hybrid-array/pull/68
[#85]: https://github.com/RustCrypto/hybrid-array/pull/85
[#86]: https://github.com/RustCrypto/hybrid-array/pull/86
[#88]: https://github.com/RustCrypto/hybrid-array/pull/88
[#99]: https://github.com/RustCrypto/hybrid-array/pull/99

## 0.1.0 (2022-05-07)
- Initial release
