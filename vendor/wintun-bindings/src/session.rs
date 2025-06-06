use crate::{
    handle::{SafeEvent, UnsafeHandle},
    packet, util, wintun_raw, Adapter, Error, Wintun,
};
use std::{ptr, slice, sync::Arc, sync::OnceLock};
use windows_sys::Win32::{
    Foundation::{GetLastError, ERROR_NO_MORE_ITEMS, FALSE, HANDLE, WAIT_EVENT, WAIT_FAILED, WAIT_OBJECT_0},
    System::Threading::{WaitForMultipleObjects, INFINITE},
};

/// Wrapper around a <https://git.zx2c4.com/wintun/about/#wintun_session_handle>
pub struct Session {
    /// The session handle given to us by WintunStartSession
    pub(crate) inner: UnsafeHandle<wintun_raw::WINTUN_SESSION_HANDLE>,

    /// Windows event handle that is signaled by the wintun driver when data becomes available to
    /// read
    pub(crate) read_event: OnceLock<UnsafeHandle<HANDLE>>,

    /// Windows event handle that is signaled when [`Session::shutdown`] is called force blocking
    /// readers to exit
    pub(crate) shutdown_event: Arc<SafeEvent>,

    /// The adapter that owns this session
    pub(crate) adapter: Arc<Adapter>,
}

impl Session {
    pub fn get_adapter(&self) -> Arc<Adapter> {
        self.adapter.clone()
    }

    pub(crate) fn get_wintun(&self) -> Wintun {
        self.adapter.wintun.clone()
    }

    /// Allocates a send packet of the specified size. Wraps WintunAllocateSendPacket
    ///
    /// All packets returned from this function must be sent using [`Session::send_packet`] because
    /// wintun establishes the send packet order based on the invocation order of this function.
    /// Therefore if a packet is allocated using this function, and then never sent, it will hold
    /// up the send queue for all other packets allocated in the future. It is okay for the session
    /// to shutdown with allocated packets that have not yet been sent
    pub fn allocate_send_packet(self: &Arc<Self>, size: u16) -> Result<packet::Packet, Error> {
        let wintun = self.get_wintun();
        let ptr = unsafe { wintun.WintunAllocateSendPacket(self.inner.0, size as u32) };
        if ptr.is_null() {
            return Err(util::get_last_error()?.into());
        }
        Ok(packet::Packet {
            //SAFETY: ptr is non null, aligned for u8, and readable for up to size bytes (which
            //must be less than isize::MAX because bytes is a u16
            bytes: unsafe { slice::from_raw_parts_mut(ptr, size as usize) },
            session: self.clone(),
            kind: packet::Kind::SendPacketPending,
        })
    }

    /// Sends a packet previously allocated with [`Session::allocate_send_packet`]
    pub fn send_packet(&self, mut packet: packet::Packet) {
        assert!(matches!(packet.kind, packet::Kind::SendPacketPending));

        let wintun = self.get_wintun();
        unsafe { wintun.WintunSendPacket(self.inner.0, packet.bytes.as_ptr()) };
        //Mark the packet at sent
        packet.kind = packet::Kind::SendPacketSent;
    }

    /// Attempts to receive a packet from the virtual interface without blocking.
    /// If there are no packets currently in the receive queue, this function returns Ok(None)
    /// without blocking. If blocking until a packet is desirable, use [`Session::receive_blocking`]
    pub fn try_receive(self: &Arc<Self>) -> Result<Option<packet::Packet>, Error> {
        let mut size = 0u32;

        let wintun = self.get_wintun();
        let ptr = unsafe { wintun.WintunReceivePacket(self.inner.0, &mut size as *mut u32) };

        debug_assert!(size <= u16::MAX as u32);
        if ptr.is_null() {
            //Wintun returns ERROR_NO_MORE_ITEMS instead of blocking if packets are not available
            return match unsafe { GetLastError() } {
                ERROR_NO_MORE_ITEMS => Ok(None),
                e => Err(std::io::Error::from_raw_os_error(e as i32).into()),
            };
        }
        Ok(Some(packet::Packet {
            kind: packet::Kind::ReceivePacket,
            //SAFETY: ptr is non null, aligned for u8, and readable for up to size bytes (which
            //must be less than isize::MAX because bytes is a u16
            bytes: unsafe { slice::from_raw_parts_mut(ptr, size as usize) },
            session: self.clone(),
        }))
    }

    /// # Safety
    /// Returns the low level read event handle that is signaled when more data becomes available
    /// to read
    pub fn get_read_wait_event(&self) -> Result<UnsafeHandle<HANDLE>, Error> {
        let wintun = self.get_wintun();
        Ok(*self
            .read_event
            .get_or_init(|| UnsafeHandle(unsafe { wintun.WintunGetReadWaitEvent(self.inner.0) })))
    }

    pub fn get_shutdown_event(&self) -> UnsafeHandle<HANDLE> {
        self.shutdown_event.get_handle()
    }

    /// Blocks until a packet is available, returning the next packet in the receive queue once this happens.
    /// If the session is closed via [`Session::shutdown`] all threads currently blocking inside this function
    /// will return Err(())
    pub fn receive_blocking(self: &Arc<Self>) -> Result<packet::Packet, Error> {
        loop {
            //Try 5 times to receive without blocking so we don't have to issue a syscall to wait
            //for the event if packets are being received at a rapid rate
            for _ in 0..5 {
                match self.try_receive() {
                    Err(err) => return Err(err),
                    Ok(Some(packet)) => return Ok(packet),
                    Ok(None) => {
                        //Try again
                        continue;
                    }
                }
            }
            self.wait_read()?;
        }
    }

    pub fn wait_read(&self) -> Result<(), Error> {
        //Wait on both the read handle and the shutdown handle so that we stop when requested
        let handles = [self.get_read_wait_event()?.0, self.shutdown_event.0 .0];
        let result = unsafe {
            //SAFETY: We abide by the requirements of WaitForMultipleObjects, handles is a
            //pointer to valid, aligned, stack memory
            WaitForMultipleObjects(handles.len() as u32, &handles as _, FALSE, INFINITE)
        };
        const WAIT_OBJECT_1: WAIT_EVENT = WAIT_OBJECT_0 + 1;
        match result {
            WAIT_FAILED => Err(util::get_last_error()?.into()),
            WAIT_OBJECT_0 => {
                //We have data!
                Ok(())
            }
            WAIT_OBJECT_1 => {
                //Shutdown event triggered
                Err(Error::ShuttingDown)
            }
            _ => {
                //This should never happen
                panic!("WaitForMultipleObjects returned unexpected value {:?}", result);
            }
        }
    }

    /// Cancels any active calls to [`Session::receive_blocking`] making them instantly return Err(_) so that session can be shutdown cleanly
    pub fn shutdown(&self) -> Result<(), Error> {
        self.shutdown_event.set_event()?;
        Ok(())
    }
}

impl Session {
    pub fn try_recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut size = 0u32;

        let wintun = &self.adapter.wintun;
        let ptr = unsafe { wintun.WintunReceivePacket(self.inner.0, &mut size as *mut u32) };

        debug_assert!(size <= u16::MAX as u32);
        if ptr.is_null() {
            // Wintun returns ERROR_NO_MORE_ITEMS instead of blocking if packets are not available
            return match unsafe { GetLastError() } {
                ERROR_NO_MORE_ITEMS => Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
                e => Err(std::io::Error::from_raw_os_error(e as i32)),
            };
        }
        let size = size as usize;
        if size > buf.len() {
            unsafe { wintun.WintunReleaseReceivePacket(self.inner.0, ptr) };
            use std::io::{Error, ErrorKind::InvalidInput};
            return Err(Error::new(InvalidInput, "destination buffer too small"));
        }
        unsafe { ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), size) };
        unsafe { wintun.WintunReleaseReceivePacket(self.inner.0, ptr) };
        Ok(size)
    }

    /// Blocks until a packet is available, returning the next packet in the receive queue once this happens.
    /// If the session is closed via [`Session::shutdown`] all threads currently blocking inside this function
    /// will return Err(())
    pub fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            // Try 5 times to receive without blocking so we don't have to issue a syscall to wait
            // for the event if packets are being received at a rapid rate
            for _ in 0..5 {
                return match self.try_recv(buf) {
                    Ok(len) => Ok(len),
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::WouldBlock {
                            // Try again
                            continue;
                        }
                        Err(e)
                    }
                };
            }
            self.wait_read()?;
        }
    }

    pub fn send(&self, buf: &[u8]) -> std::io::Result<usize> {
        let wintun = &self.adapter.wintun;
        let size = buf.len();
        let ptr = unsafe { wintun.WintunAllocateSendPacket(self.inner.0, size as u32) };
        if ptr.is_null() {
            util::get_last_error()?;
        }
        unsafe { ptr::copy_nonoverlapping(buf.as_ptr(), ptr, size) };
        unsafe { wintun.WintunSendPacket(self.inner.0, ptr) };
        Ok(buf.len())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(e) = self.shutdown() {
            log::trace!("Failed to shutdown session: {}", e);
        }
        unsafe { self.get_wintun().WintunEndSession(self.inner.0) };
        self.inner.0 = ptr::null_mut();
    }
}
