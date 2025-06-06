use windows_sys::core::GUID;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    ConvertInterfaceAliasToLuid, ConvertInterfaceLuidToAlias, ConvertInterfaceLuidToGuid, ConvertInterfaceLuidToIndex,
};
use windows_sys::Win32::NetworkManagement::Ndis::{IF_MAX_STRING_SIZE, NET_LUID_LH};

pub fn luid_to_alias(luid: &NET_LUID_LH) -> std::io::Result<String> {
    let mut alias = vec![0; IF_MAX_STRING_SIZE as usize + 1];

    let r = match unsafe { ConvertInterfaceLuidToAlias(luid, alias.as_mut_ptr(), alias.len()) } {
        0 => alias,
        err => return Err(std::io::Error::from_raw_os_error(err as _)),
    };
    Ok(crate::util::decode_utf16(&r))
}

pub fn alias_to_luid(alias: &str) -> std::io::Result<NET_LUID_LH> {
    let alias = alias.encode_utf16().chain(std::iter::once(0)).collect::<Vec<_>>();
    let mut luid = unsafe { std::mem::zeroed() };

    match unsafe { ConvertInterfaceAliasToLuid(alias.as_ptr(), &mut luid) } {
        0 => Ok(luid),
        err => Err(std::io::Error::from_raw_os_error(err as _)),
    }
}
pub fn luid_to_index(luid: &NET_LUID_LH) -> std::io::Result<u32> {
    let mut index = 0;

    match unsafe { ConvertInterfaceLuidToIndex(luid, &mut index) } {
        0 => Ok(index),
        err => Err(std::io::Error::from_raw_os_error(err as _)),
    }
}

pub fn luid_to_guid(luid: &NET_LUID_LH) -> std::io::Result<GUID> {
    let mut guid = unsafe { std::mem::zeroed() };

    match unsafe { ConvertInterfaceLuidToGuid(luid, &mut guid) } {
        0 => Ok(guid),
        err => Err(std::io::Error::from_raw_os_error(err as _)),
    }
}
