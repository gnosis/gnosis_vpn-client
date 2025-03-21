#[cfg(any(freebsdlike, linux_kernel, target_os = "fuchsia"))]
pub(crate) mod cpu_set;
#[cfg(linux_kernel)]
pub(crate) mod futex;
#[cfg(not(windows))]
pub(crate) mod syscalls;
pub(crate) mod types;
