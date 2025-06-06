pub trait FnCast: Copy {}

pub struct FnHolder<F: FnCast> {
    _lib: ::libloading::Library,
    pub func: F,
}

unsafe impl<F: FnCast> Send for FnHolder<F> {}
unsafe impl<F: FnCast> Sync for FnHolder<F> {}

pub fn load_function<F: FnCast>(module_name: &str, function_name: &str) -> Option<FnHolder<F>> {
    unsafe {
        let function_name_cstr = std::ffi::CString::new(function_name).ok()?;
        let _lib = ::libloading::Library::new(module_name).ok()?;
        let func: F = _lib.get(function_name_cstr.to_bytes_with_nul()).map(|sym| *sym).ok()?;
        Some(FnHolder { _lib, func })
    }
}

#[macro_export]
macro_rules! define_fn_dynamic_load {
    ($fn_type:ident, $fn_signature:ty, $static_var:ident, $load_fn:ident, $module_name:expr, $fn_name:expr) => {
        pub type $fn_type = $fn_signature;

        impl $crate::fn_holder::FnCast for $fn_type {}

        #[allow(non_upper_case_globals)]
        static $static_var: std::sync::OnceLock<Option<$crate::fn_holder::FnHolder<$fn_type>>> =
            std::sync::OnceLock::new();

        #[allow(non_snake_case)]
        pub fn $load_fn() -> Option<$fn_type> {
            $static_var
                .get_or_init(|| $crate::fn_holder::load_function($module_name, $fn_name))
                .as_ref()
                .map(|fn_holder| fn_holder.func)
        }
    };
}

/*
// usage
use windows_sys::Win32::Foundation::BOOL;
define_fn_dynamic_load!(
    ProcessPrngDeclare,
    unsafe extern "system" fn(pbdata: *mut u8, cbdata: usize) -> BOOL,
    PROCESS_PRNG,
    ProcessPrng,
    "bcryptprimitives.dll",
    "ProcessPrng"
);
let func = ProcessPrng().ok_or("Failed to load function ProcessPrng")?;
*/
