/// Representation of a winton adapter with safe idiomatic bindings to the functionality provided by
/// the WintunAdapter* C functions.
///
/// The [`Adapter::create`] and [`Adapter::open`] functions serve as the entry point to using
/// wintun functionality
use crate::{
    error::{Error, OutOfRangeData},
    handle::{SafeEvent, UnsafeHandle},
    session::Session,
    util::{self},
    wintun_raw, Wintun,
};
use std::{
    ffi::OsStr,
    net::{IpAddr, Ipv4Addr},
    os::windows::prelude::OsStrExt,
    ptr,
    sync::Arc,
    sync::OnceLock,
};
use windows_sys::{
    core::GUID,
    Win32::NetworkManagement::{IpHelper::ConvertLengthToIpv4Mask, Ndis::NET_LUID_LH},
};

/// Wrapper around a <https://git.zx2c4.com/wintun/about/#wintun_adapter_handle>
pub struct Adapter {
    adapter: UnsafeHandle<wintun_raw::WINTUN_ADAPTER_HANDLE>,
    pub(crate) wintun: Wintun,
    guid: u128,
    index: u32,
    luid: NET_LUID_LH,
}

impl Adapter {
    /// Returns the `Friendly Name` of this adapter,
    /// which is the human readable name shown in Windows
    pub fn get_name(&self) -> Result<String, Error> {
        Ok(crate::ffi::luid_to_alias(&self.luid)?)
    }

    /// Sets the `Friendly Name` of this adapter,
    /// which is the human readable name shown in Windows
    ///
    /// Note: This is different from `Adapter Name`, which is a GUID.
    pub fn set_name(&self, name: &str) -> Result<(), Error> {
        // use command `netsh interface set interface name="oldname" newname="mynewname"`

        let args = &[
            "interface",
            "set",
            "interface",
            &format!("name=\"{}\"", self.get_name()?),
            &format!("newname=\"{}\"", name),
        ];
        util::run_command("netsh", args)?;

        Ok(())
    }

    pub fn get_guid(&self) -> u128 {
        self.guid
    }

    /// Creates a new wintun adapter inside the name `name` with tunnel type `tunnel_type`
    ///
    /// Optionally a GUID can be specified that will become the GUID of this adapter once created.
    pub fn create(wintun: &Wintun, name: &str, tunnel_type: &str, guid: Option<u128>) -> Result<Arc<Adapter>, Error> {
        let name_utf16: Vec<_> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let tunnel_type_utf16: Vec<u16> = tunnel_type.encode_utf16().chain(std::iter::once(0)).collect();

        let mut guid = match guid {
            Some(guid) => guid,
            None => {
                let mut guid: GUID = unsafe { std::mem::zeroed() };
                unsafe { windows_sys::Win32::System::Rpc::UuidCreate(&mut guid as *mut GUID) };
                util::win_guid_to_u128(&guid)
            }
        };

        crate::log::set_default_logger_if_unset(wintun);

        let guid_s: GUID = GUID::from_u128(guid);
        let result = unsafe { wintun.WintunCreateAdapter(name_utf16.as_ptr(), tunnel_type_utf16.as_ptr(), &guid_s) };

        if result.is_null() {
            return crate::log::extract_wintun_log_error("WintunCreateAdapter failed")?;
        }
        let mut call = || -> Result<Arc<Adapter>, Error> {
            let luid = crate::ffi::alias_to_luid(name)?;
            let index = crate::ffi::luid_to_index(&luid)?;
            let real_guid = util::win_guid_to_u128(&crate::ffi::luid_to_guid(&luid)?);
            if guid != real_guid {
                let real_guid_s = util::guid_to_win_style_string(&GUID::from_u128(real_guid))?;
                let guid_s = util::guid_to_win_style_string(&GUID::from_u128(guid))?;
                let (major, minor, build) = util::get_windows_version()?;
                log::warn!("Windows {major}.{minor}.{build} internal bug cause the GUID mismatch: Expected {guid_s}, got {real_guid_s}");
                guid = real_guid;
            }
            Ok(Arc::new(Adapter {
                adapter: UnsafeHandle(result),
                wintun: wintun.clone(),
                guid,
                index,
                luid,
            }))
        };
        match call() {
            Ok(adapter) => Ok(adapter),
            Err(e) => {
                unsafe { wintun.WintunCloseAdapter(result) };
                Err(e)
            }
        }
    }

    /// Attempts to open an existing wintun interface name `name`.
    pub fn open(wintun: &Wintun, name: &str) -> Result<Arc<Adapter>, Error> {
        let name_utf16: Vec<u16> = OsStr::new(name).encode_wide().chain(std::iter::once(0)).collect();

        crate::log::set_default_logger_if_unset(wintun);

        let result = unsafe { wintun.WintunOpenAdapter(name_utf16.as_ptr()) };

        if result.is_null() {
            return crate::log::extract_wintun_log_error("WintunOpenAdapter failed")?;
        }
        let call = || -> Result<Arc<Adapter>, Error> {
            let luid = crate::ffi::alias_to_luid(name)?;
            let index = crate::ffi::luid_to_index(&luid)?;
            let guid = crate::ffi::luid_to_guid(&luid)?;
            let guid = util::win_guid_to_u128(&guid);
            Ok(Arc::new(Adapter {
                adapter: UnsafeHandle(result),
                wintun: wintun.clone(),
                guid,
                index,
                luid,
            }))
        };
        match call() {
            Ok(adapter) => Ok(adapter),
            Err(e) => {
                unsafe { wintun.WintunCloseAdapter(result) };
                Err(e)
            }
        }
    }

    /// Delete an adapter, consuming it in the process
    pub fn delete(self) -> Result<(), Error> {
        //Dropping an adapter closes it
        drop(self);
        // Return a result here so that if later the API changes to be fallible, we can support it
        // without making a breaking change
        Ok(())
    }

    fn validate_capacity(capacity: u32) -> Result<(), Error> {
        let range = crate::MIN_RING_CAPACITY..=crate::MAX_RING_CAPACITY;
        if !range.contains(&capacity) {
            return Err(Error::CapacityOutOfRange(OutOfRangeData { range, value: capacity }));
        }
        if !capacity.is_power_of_two() {
            return Err(Error::CapacityNotPowerOfTwo(capacity));
        }
        Ok(())
    }

    /// Initiates a new wintun session on the given adapter.
    ///
    /// Capacity is the size in bytes of the ring buffer used internally by the driver. Must be
    /// a power of two between [`crate::MIN_RING_CAPACITY`] and [`crate::MAX_RING_CAPACITY`] inclusive.
    pub fn start_session(self: &Arc<Self>, capacity: u32) -> Result<Arc<Session>, Error> {
        Self::validate_capacity(capacity)?;

        let result = unsafe { self.wintun.WintunStartSession(self.adapter.0, capacity) };

        if result.is_null() {
            return crate::log::extract_wintun_log_error("WintunStartSession failed")?;
        }
        // Manual reset, because we use this event once and it must fire on all threads
        let shutdown_event = SafeEvent::new(true, false)?;
        Ok(Arc::new(Session {
            inner: UnsafeHandle(result),
            read_event: OnceLock::new(),
            shutdown_event: Arc::new(shutdown_event),
            adapter: self.clone(),
        }))
    }

    /// Returns the Win32 LUID for this adapter
    pub fn get_luid(&self) -> NET_LUID_LH {
        self.luid
    }

    /// Set `MTU` of this adapter
    pub fn set_mtu(&self, mtu: usize) -> Result<(), Error> {
        let name = self.get_name()?;
        util::set_adapter_mtu(&name, mtu, false)?;
        // FIXME: Here we set the IPv6 MTU as well for consistency, but for some users it may not be expected.
        util::set_adapter_mtu(&name, mtu, true)?;
        Ok(())
    }

    /// Returns `MTU` of this adapter
    pub fn get_mtu(&self) -> Result<usize, Error> {
        // FIXME: Here we get the IPv4 MTU only, but for some users it may not be expected.
        Ok(util::get_mtu_by_index(self.index, false)? as _)
    }

    /// Returns the Win32 interface index of this adapter. Useful for specifying the interface
    /// when executing `netsh interface ip` commands
    pub fn get_adapter_index(&self) -> Result<u32, Error> {
        Ok(self.index)
    }

    /// Sets the IP address for this adapter, using command `netsh`.
    pub fn set_address(&self, address: Ipv4Addr) -> Result<(), Error> {
        let binding = self.get_addresses()?;
        let old_address = binding.iter().find(|addr| matches!(addr, IpAddr::V4(_)));
        let mask = match old_address {
            Some(IpAddr::V4(addr)) => self.get_netmask_of_address(&(*addr).into())?,
            _ => "255.255.255.0".parse()?,
        };
        let gateway = self
            .get_gateways()?
            .iter()
            .find(|addr| matches!(addr, IpAddr::V4(_)))
            .cloned();
        self.set_network_addresses_tuple(address.into(), mask, gateway)?;
        Ok(())
    }

    /// Sets the gateway for this adapter, using command `netsh`.
    pub fn set_gateway(&self, gateway: Option<Ipv4Addr>) -> Result<(), Error> {
        let binding = self.get_addresses()?;
        let address = binding.iter().find(|addr| matches!(addr, IpAddr::V4(_)));
        let address = match address {
            Some(IpAddr::V4(addr)) => addr,
            _ => return Err("Unable to find IPv4 address".into()),
        };
        let mask = self.get_netmask_of_address(&(*address).into())?;
        let gateway = gateway.map(|addr| addr.into());
        self.set_network_addresses_tuple((*address).into(), mask, gateway)?;
        Ok(())
    }

    /// Sets the subnet mask for this adapter, using command `netsh`.
    pub fn set_netmask(&self, mask: Ipv4Addr) -> Result<(), Error> {
        let binding = self.get_addresses()?;
        let address = binding.iter().find(|addr| matches!(addr, IpAddr::V4(_)));
        let address = match address {
            Some(IpAddr::V4(addr)) => addr,
            _ => return Err("Unable to find IPv4 address".into()),
        };
        let gateway = self
            .get_gateways()?
            .iter()
            .find(|addr| matches!(addr, IpAddr::V4(_)))
            .cloned();
        self.set_network_addresses_tuple((*address).into(), mask.into(), gateway)?;
        Ok(())
    }

    /// Sets the DNS servers for this adapter
    pub fn set_dns_servers(&self, dns_servers: &[IpAddr]) -> Result<(), Error> {
        let interface = GUID::from_u128(self.get_guid());
        if let Err(e) = util::set_interface_dns_servers(interface, dns_servers) {
            log::debug!("Failed to set DNS servers in first attempt: \"{}\", try another...", e);
            util::set_interface_dns_servers_via_cmd(&self.get_name()?, dns_servers)?;
        }
        Ok(())
    }

    /// Sets the network addresses of this adapter, including network address, subnet mask, and gateway
    pub fn set_network_addresses_tuple(
        &self,
        address: IpAddr,
        mask: IpAddr,
        gateway: Option<IpAddr>,
    ) -> Result<(), Error> {
        let name = self.get_name()?;
        // command line: `netsh interface ipv4 set address name="YOUR_INTERFACE_NAME" source=static address=IP_ADDRESS mask=SUBNET_MASK gateway=GATEWAY`
        // or shorter command: `netsh interface ipv4 set address name="YOUR_INTERFACE_NAME" static IP_ADDRESS SUBNET_MASK GATEWAY`
        // for example: `netsh interface ipv4 set address name="Wi-Fi" static 192.168.3.8 255.255.255.0 192.168.3.1`
        let mut args: Vec<String> = vec![
            "interface".into(),
            if address.is_ipv4() {
                "ipv4".into()
            } else {
                "ipv6".into()
            },
            "set".into(),
            "address".into(),
            format!("name=\"{}\"", name),
            "source=static".into(),
            format!("address={}", address),
            format!("mask={}", mask),
        ];
        if let Some(gateway) = gateway {
            args.push(format!("gateway={}", gateway));
        }
        util::run_command("netsh", &args.iter().map(|s| s.as_str()).collect::<Vec<&str>>())?;
        Ok(())
    }

    /// Returns the IP addresses of this adapter, including IPv4 and IPv6 addresses
    pub fn get_addresses(&self) -> Result<Vec<IpAddr>, Error> {
        let name = util::guid_to_win_style_string(&GUID::from_u128(self.guid))?;

        let mut adapter_addresses = vec![];

        util::get_adapters_addresses(|adapter| {
            let name_iter = match unsafe { util::win_pstr_to_string(adapter.AdapterName) } {
                Ok(name) => name,
                Err(err) => {
                    log::error!("Failed to parse adapter name: {}", err);
                    return false;
                }
            };
            if name_iter == name {
                let mut current_address = adapter.FirstUnicastAddress;
                while !current_address.is_null() {
                    let address = unsafe { (*current_address).Address };
                    match util::retrieve_ipaddr_from_socket_address(&address) {
                        Ok(addr) => adapter_addresses.push(addr),
                        Err(err) => {
                            log::error!("Failed to parse address: {}", err);
                        }
                    }
                    unsafe { current_address = (*current_address).Next };
                }
            }
            true
        })?;

        Ok(adapter_addresses)
    }

    /// Returns the gateway addresses of this adapter, including IPv4 and IPv6 addresses
    pub fn get_gateways(&self) -> Result<Vec<IpAddr>, Error> {
        let name = util::guid_to_win_style_string(&GUID::from_u128(self.guid))?;
        let mut gateways = vec![];
        util::get_adapters_addresses(|adapter| {
            let name_iter = match unsafe { util::win_pstr_to_string(adapter.AdapterName) } {
                Ok(name) => name,
                Err(err) => {
                    log::error!("Failed to parse adapter name: {}", err);
                    return false;
                }
            };
            if name_iter == name {
                let mut current_gateway = adapter.FirstGatewayAddress;
                while !current_gateway.is_null() {
                    let gateway = unsafe { (*current_gateway).Address };
                    match util::retrieve_ipaddr_from_socket_address(&gateway) {
                        Ok(addr) => gateways.push(addr),
                        Err(err) => {
                            log::error!("Failed to parse gateway: {}", err);
                        }
                    }
                    unsafe { current_gateway = (*current_gateway).Next };
                }
            }
            true
        })?;
        Ok(gateways)
    }

    /// Returns the subnet mask of the given address
    pub fn get_netmask_of_address(&self, target_address: &IpAddr) -> Result<IpAddr, Error> {
        let name = util::guid_to_win_style_string(&GUID::from_u128(self.guid))?;
        let mut subnet_mask = None;
        util::get_adapters_addresses(|adapter| {
            let name_iter = match unsafe { util::win_pstr_to_string(adapter.AdapterName) } {
                Ok(name) => name,
                Err(err) => {
                    log::warn!("Failed to parse adapter name: {}", err);
                    return false;
                }
            };
            if name_iter == name {
                let mut current_address = adapter.FirstUnicastAddress;
                while !current_address.is_null() {
                    let address = unsafe { (*current_address).Address };
                    let address = match util::retrieve_ipaddr_from_socket_address(&address) {
                        Ok(addr) => addr,
                        Err(err) => {
                            log::warn!("Failed to parse address: {}", err);
                            return false;
                        }
                    };
                    if address == *target_address {
                        let masklength = unsafe { (*current_address).OnLinkPrefixLength };
                        match address {
                            IpAddr::V4(_) => {
                                let mut mask = 0_u32;
                                match unsafe { ConvertLengthToIpv4Mask(masklength as u32, &mut mask as *mut u32) } {
                                    0 => {}
                                    err => {
                                        log::warn!("Failed to convert length to mask: {}", err);
                                        return false;
                                    }
                                }
                                subnet_mask = Some(IpAddr::V4(Ipv4Addr::from(mask.to_le_bytes())));
                            }
                            IpAddr::V6(_) => match util::ipv6_netmask_for_prefix(masklength) {
                                Ok(v) => subnet_mask = Some(IpAddr::V6(v)),
                                Err(err) => {
                                    log::warn!("Failed to convert length to mask: {}", err);
                                    return false;
                                }
                            },
                        }
                        break;
                    }
                    unsafe { current_address = (*current_address).Next };
                }
            }
            true
        })?;

        Ok(subnet_mask.ok_or("Unable to find matching address")?)
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        let _name = self.get_name();
        //Close adapter on drop
        //This is why we need an Arc of wintun
        unsafe { self.wintun.WintunCloseAdapter(self.adapter.0) };
        self.adapter = UnsafeHandle(ptr::null_mut());
        #[cfg(feature = "winreg")]
        if let Ok(name) = _name {
            // Delete registry related to network card
            _ = delete_adapter_info_from_reg(&name);
        }
    }
}

/// This function is used to avoid the adapter name and guid being recorded in the registry
#[cfg(feature = "winreg")]
pub(crate) fn delete_adapter_info_from_reg(dev_name: &str) -> std::io::Result<()> {
    use winreg::{enums::HKEY_LOCAL_MACHINE, enums::KEY_ALL_ACCESS, RegKey};
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let profiles_key = hklm.open_subkey_with_flags(
        "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\NetworkList\\Profiles",
        KEY_ALL_ACCESS,
    )?;

    for sub_key_name in profiles_key.enum_keys().filter_map(Result::ok) {
        let sub_key = profiles_key.open_subkey(&sub_key_name)?;
        match sub_key.get_value::<String, _>("ProfileName") {
            Ok(profile_name) => {
                if dev_name == profile_name {
                    match profiles_key.delete_subkey_all(&sub_key_name) {
                        Ok(_) => log::info!("Successfully deleted Profiles sub_key: {}", sub_key_name),
                        Err(e) => log::warn!("Failed to delete Profiles sub_key {}: {}", sub_key_name, e),
                    }
                }
            }
            Err(e) => log::warn!("Failed to read ProfileName for sub_key {}: {}", sub_key_name, e),
        }
    }
    let unmanaged_key = hklm.open_subkey_with_flags(
        "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\NetworkList\\Signatures\\Unmanaged",
        KEY_ALL_ACCESS,
    )?;
    for sub_key_name in unmanaged_key.enum_keys().filter_map(Result::ok) {
        let sub_key = unmanaged_key.open_subkey(&sub_key_name)?;
        match sub_key.get_value::<String, _>("Description") {
            Ok(description) => {
                if dev_name == description {
                    match unmanaged_key.delete_subkey_all(&sub_key_name) {
                        Ok(_) => log::info!("Successfully deleted Unmanaged sub_key: {}", sub_key_name),
                        Err(e) => log::warn!("Failed to delete Unmanaged sub_key {}: {}", sub_key_name, e),
                    }
                }
            }
            Err(e) => log::warn!("Failed to read Description for sub_key {}: {}", sub_key_name, e),
        }
    }
    Ok(())
}
