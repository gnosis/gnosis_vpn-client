use crate::{handle::UnsafeHandle, session::Session};
use futures::{AsyncRead, AsyncWrite};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use windows_sys::Win32::{
    Foundation::{FALSE, HANDLE, WAIT_ABANDONED_0, WAIT_EVENT, WAIT_OBJECT_0},
    System::Threading::{WaitForMultipleObjects, INFINITE},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitingStopReason {
    Shutdown,
    Ready,
}

#[derive(Debug, Clone)]
enum ReadState {
    Waiting(Option<Arc<Mutex<blocking::Task<WaitingStopReason>>>>),
    Idle,
    Closed,
}

#[derive(Clone)]
pub struct AsyncSession {
    session: Arc<Session>,
    read_state: ReadState,
}

impl std::ops::Deref for AsyncSession {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

impl From<Arc<Session>> for AsyncSession {
    fn from(session: Arc<Session>) -> Self {
        Self {
            session,
            read_state: ReadState::Idle,
        }
    }
}

impl AsyncSession {
    fn wait_for_read(read_event: UnsafeHandle<HANDLE>, shutdown_event: UnsafeHandle<HANDLE>) -> WaitingStopReason {
        const WAIT_OBJECT_1: WAIT_EVENT = WAIT_OBJECT_0 + 1;
        const WAIT_ABANDONED_1: WAIT_EVENT = WAIT_ABANDONED_0 + 1;
        let handles = [shutdown_event.0, read_event.0];
        match unsafe { WaitForMultipleObjects(handles.len() as u32, &handles as _, FALSE, INFINITE) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED_0 => WaitingStopReason::Shutdown,
            WAIT_OBJECT_1 => WaitingStopReason::Ready,
            WAIT_ABANDONED_1 => panic!("Read event deleted unexpectedly"),
            e => panic!("WaitForMultipleObjects returned unexpected value {:?}", e),
        }
    }

    pub async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.session.try_receive() {
                Ok(Some(packet)) => {
                    let size = packet.bytes.len();
                    if buf.len() < size {
                        return Err(std::io::Error::new(std::io::ErrorKind::Other, "Buffer too small"));
                    }
                    buf[..size].copy_from_slice(&packet.bytes[..size]);
                    return Ok(size);
                }
                Ok(None) => {
                    let read_event = self.session.get_read_wait_event()?;
                    let shutdown_event = self.session.shutdown_event.get_handle();
                    match blocking::unblock(move || Self::wait_for_read(read_event, shutdown_event)).await {
                        WaitingStopReason::Shutdown => {
                            return Err(crate::Error::ShuttingDown.into());
                        }
                        WaitingStopReason::Ready => continue,
                    }
                }
                Err(err) => return Err(std::io::Error::new(std::io::ErrorKind::Other, err)),
            }
        }
    }

    pub async fn send(&self, buf: &[u8]) -> std::io::Result<usize> {
        self.internal_send(buf)
    }

    fn internal_send(&self, buf: &[u8]) -> std::io::Result<usize> {
        let packet = self.session.allocate_send_packet(buf.len() as _)?;
        packet.bytes.copy_from_slice(buf);
        self.session.send_packet(packet);
        Ok(buf.len())
    }
}

impl AsyncRead for AsyncSession {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context, buf: &mut [u8]) -> Poll<std::io::Result<usize>> {
        use std::io::{Error, ErrorKind::Other};
        loop {
            match &mut self.read_state {
                ReadState::Idle => match self.session.try_receive() {
                    Ok(Some(packet)) => {
                        let size = packet.bytes.len();
                        if buf.len() < size {
                            return Poll::Ready(Err(Error::new(Other, "Buffer too small")));
                        }
                        buf[..size].copy_from_slice(&packet.bytes[..size]);
                        return Poll::Ready(Ok(size));
                    }
                    Ok(None) => {
                        let read_event = self.session.get_read_wait_event()?;
                        let shutdown_event = self.session.shutdown_event.get_handle();
                        let task = Arc::new(Mutex::new(blocking::unblock(move || {
                            Self::wait_for_read(read_event, shutdown_event)
                        })));
                        self.read_state = ReadState::Waiting(Some(task));
                    }
                    Err(err) => return Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, err))),
                },
                ReadState::Waiting(task) => {
                    let task = match task.take() {
                        Some(task) => task,
                        None => return Poll::Pending,
                    };
                    let task_clone = task.clone();
                    let mut task_guard = match task_clone.lock() {
                        Ok(guard) => guard,
                        Err(e) => {
                            self.read_state = ReadState::Waiting(Some(task));
                            return Poll::Ready(Err(Error::new(Other, format!("Lock task failed: {}", e))));
                        }
                    };
                    self.read_state = match Pin::new(&mut *task_guard).poll(cx) {
                        Poll::Ready(WaitingStopReason::Shutdown) => ReadState::Closed,
                        Poll::Ready(WaitingStopReason::Ready) => ReadState::Idle,
                        Poll::Pending => ReadState::Waiting(Some(task)),
                    };
                    if let ReadState::Waiting(_) = self.read_state {
                        return Poll::Pending;
                    }
                }
                ReadState::Closed => return Poll::Ready(Ok(0)),
            }
        }
    }
}

impl AsyncWrite for AsyncSession {
    fn poll_write(self: Pin<&mut Self>, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Ok(self.internal_send(buf)?))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.session.shutdown()?;
        Poll::Ready(Ok(()))
    }
}
