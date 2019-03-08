//! There's a few gotchas to be aware of when using this library:
//!
//! 1. Linux AIO requires the underlying file to be opened in direct mode (`O_DIRECT`), bypassing
//! any other buffering at the OS level. If you attempt to use this library on files opened regularly,
//! likely it won't work.
//!
//! 2. Because Linux AIO operates on files in direct mode, by corrollary the memory buffers associated
//! with read/write requests need to be suitable for direct DMA transfers. This means that those buffers
//! should be aligned to hardware page boundaries, and the memory needs to be mapped to pysical RAM.
//! The best way to accomplish this is to have a mmapped region that is locked in physical memory.
//!
//! 3. Due to the asynchronous nature of this library, memory buffers are represented using generic
//! handle types. For the purpose of the inner workings of this library, the important aspect is that
//! those handle types can be dereferenced into a `&[u8]` or, respectively, a `&mut [u8]` type. Because
//! we hand off those buffers to the kernel (and ultimately hardware DMA) it is mandatory that those
//! bytes slices have a fixed address in main memory during I/O processing.
//!

extern crate memmap;

use std::io::Read;
use std::mem;

use super::aio_bindings::{self as aio, c_long};
use io::sys::from_nix_error;
use io::CoIo;
use nix::sys::eventfd::{eventfd, EfdFlags};
use nix::unistd::{close, read};
use sync::Semphore;

pub struct EventFd(RawFd);

impl AsRawFd for EventFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Read for EventFd {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        read(self.0, buf).map_err(from_nix_error)
    }
}

impl Drop for EventFd {
    fn drop(&mut self) {
        close(self.0).ok();
    }
}

// field values that we need to transfer into a kernel IOCB
struct IocbInfo {
    // the I/O opcode
    opcode: u32,

    // file fd identifying the file to operate on
    fd: RawFd,

    // an absolute file offset, if applicable for the command
    offset: u64,

    // the base address of the transfer buffer, if applicable
    buf: u64,

    // the number of bytes to be transferred, if applicable
    len: u64,

    // flags to provide additional parameters
    flags: u32,
}

pub struct AioManager {
    context: aio::aio_context_t,
    events: Vec<aio::io_event>,
    event_fd: RawFd,
    sem: Semphore,
    buffers: Vec<memmap::MmapMut>,
}

impl AioManager {
    pub fn new(nr: usize) -> io::Result<Self> {
        let mut buffers = Vec::new();
        for _ in 0..nr {
            buffers.push(
                memmap::MmapMut::map_anon(4096)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, "can't alloc memmap"))?,
            );
        }

        let flags = EfdFlags::EFD_CLOEXEC | EfdFlags::EFD_NONBLOCK;
        let event_fd = eventfd(0, flags).map_err(from_nix_error)?;

        let mut context: aio::aio_context_t = 0;
        unsafe {
            if aio::io_setup(nr as c_long, &mut context) != 0 {
                return Err(io::Error::last_os_error());
            }
        };

        AioManager {
            context,
            buffers,
            event_fd,
            sem: Semphore::new(nr),
            events: Vec::with_capacity(nr),
        }
    }

    pub fn get_event_fd(&self) -> RawFd {
        self.event_fd.0
    }

    fn submit_request(&mut self) -> io::Result<()> {
        if self.state.is_none() {
            // See if we can secure a submission slot
            if self.acquire_state.is_none() {
                self.acquire_state = Some(self.context.have_capacity.acquire());
            }

            match self.acquire_state.as_mut().unwrap().poll() {
                Err(err) => return Err(err),
                Ok(futures::Async::NotReady) => return Ok(futures::Async::NotReady),
                Ok(futures::Async::Ready(_)) => {
                    // retrieve a state container from the set of available ones and move it into the future
                    let mut guard = self.context.capacity.write();
                    match guard {
                        Ok(ref mut guard) => {
                            self.state = guard.state.pop();
                        }
                        Err(_) => panic!("TODO: Figure out how to handle this kind of error"),
                    }
                }
            }

            assert!(self.state.is_some());
            let state = self.state.as_mut().unwrap();
            let state_addr = state.deref().deref() as *const RequestState;

            // Fill in the iocb data structure to be submitted to the kernel
            state.request.aio_data = unsafe { mem::transmute::<_, usize>(state_addr) } as u64;
            state.request.aio_resfd = self.context.completed_fd as u32;
            state.request.aio_flags = aio::IOCB_FLAG_RESFD | self.iocb_info.flags;
            state.request.aio_fildes = self.iocb_info.fd as u32;
            state.request.aio_offset = self.iocb_info.offset as i64;
            state.request.aio_buf = self.iocb_info.buf;
            state.request.aio_nbytes = self.iocb_info.len;
            state.request.aio_lio_opcode = self.iocb_info.opcode as u16;

            // attach synchronization primitives that are used to indicate completion of this request
            let (sender, receiver) = futures::sync::oneshot::channel();
            state.completed_receiver = receiver;
            state.completed_sender = Some(sender);

            // submit the request
            let mut request_ptr_array: [*mut aio::iocb; 1] =
                [&mut state.request as *mut aio::iocb; 1];

            let result = unsafe {
                aio::io_submit(
                    self.context.context,
                    1,
                    &mut request_ptr_array[0] as *mut *mut aio::iocb,
                )
            };

            // if we have submission error, capture it as future result
            if result != 1 {
                return Err(io::Error::last_os_error());
            }
        }

        Ok(futures::Async::Ready(()))
    }

    // Attempt to retrieve the result of a previously submitted I/O request; this may need to
    // wait until the I/O operation has been completed
    fn retrieve_result(&mut self) -> io::Result<()> {
        // Check if we have received a notification indicating completion of the I/O request
        let result_code = match self.state.as_mut().unwrap().completed_receiver.poll() {
            Err(err) => return Err(io::Error::new(io::ErrorKind::Other, err)),
            Ok(futures::Async::NotReady) => return Ok(futures::Async::NotReady),
            Ok(futures::Async::Ready(n)) => n,
        };

        // Release the kernel queue slot and the state variable that we just processed
        match self.context.capacity.write() {
            Ok(ref mut guard) => {
                guard.state.push(self.state.take().unwrap());
            }
            Err(_) => panic!("TODO: Figure out how to handle this kind of error"),
        }

        // notify others that we release a state slot
        self.context.have_capacity.release();

        if result_code < 0 {
            Err(io::Error::from_raw_os_error(result_code as i32))
        } else {
            Ok(futures::Async::Ready(()))
        }
    }

    /// Initiate an asynchronous write operation on the given file descriptor for writing
    /// data to the provided absolute file offset from the buffer. The buffer also determines
    /// the number of bytes to be written, which should be a multiple of the underlying device block
    /// size.
    ///
    /// # Params:
    /// - fd: The file descriptor of the file to which to write
    /// - offset: The file offset where we want to write to
    /// - buffer: A buffer holding the data to be written
    /// - sync_level: A synchronization level to apply for this write operation
    pub fn write_sync<ReadOnlyHandle>(
        &self,
        fd: RawFd,
        offset: u64,
        buffer_obj: ReadOnlyHandle,
        sync_level: SyncLevel,
    ) -> AioWriteResultFuture<ReadOnlyHandle>
    where
        ReadOnlyHandle: convert::AsRef<[u8]>,
    {
        let (ptr, len) = {
            let buffer = buffer_obj.as_ref();
            let len = buffer.len() as u64;
            let ptr = unsafe { mem::transmute::<_, usize>(buffer.as_ptr()) } as c_long;
            (ptr, len)
        };

        // nothing really happens here until someone calls poll
        AioWriteResultFuture {
            base: AioBaseFuture {
                context: self.inner.clone(),
                iocb_info: IocbInfo {
                    opcode: aio::IOCB_CMD_PWRITE,
                    fd,
                    offset,
                    len,
                    buf: ptr as u64,
                    flags: sync_level as u32,
                },
                state: None,
                acquire_state: None,
            },
            buffer: Some(buffer_obj),
        }
    }

    /// Initiate an asynchronous read operation on the given file descriptor for reading
    /// data from the provided absolute file offset into the buffer. The buffer also determines
    /// the number of bytes to be read, which should be a multiple of the underlying device block
    /// size.
    ///
    /// # Params:
    /// - fd: The file descriptor of the file from which to read
    /// - offset: The file offset where we want to read from
    /// - buffer: A buffer to receive the read results
    pub fn read<ReadWriteHandle>(
        &self,
        fd: RawFd,
        offset: u64,
        mut buffer_obj: ReadWriteHandle,
    ) -> AioReadResultFuture<ReadWriteHandle>
    where
        ReadWriteHandle: convert::AsMut<[u8]>,
    {
        let (ptr, len) = {
            let buffer = buffer_obj.as_mut();
            let len = buffer.len() as u64;
            let ptr = unsafe { mem::transmute::<_, usize>(buffer.as_ptr()) } as u64;
            (ptr, len)
        };

        // nothing really happens here until someone calls poll
        AioReadResultFuture {
            base: AioBaseFuture {
                context: self.inner.clone(),
                iocb_info: IocbInfo {
                    opcode: aio::IOCB_CMD_PREAD,
                    fd,
                    offset,
                    len,
                    buf: ptr,
                    flags: 0,
                },
                state: None,
                acquire_state: None,
            },
            buffer: Some(buffer_obj),
        }
    }

    fn aio_server(&self) -> io::Result<()> {
        let mut event_fd = CoIo::new(EventFd(self.event_fd))?;
        loop {
            let mut buf: [u8; 8] = [0; 8];
            event_fd.read(mem::transmute(&mut buf))?;
            let available: u64 = mem::transmute(buf);

            if available == 0 {
                // this should never happned
                continue;
            }

            unsafe {
                let result = aio::io_getevents(
                    self.context,
                    available as c_long,
                    available as c_long,
                    self.events.as_mut_ptr(),
                    ptr::null_mut::<aio::timespec>(),
                );

                // adjust the vector size to the actual number of items returned
                if result < 0 {
                    return Err(io::Error::last_os_error());
                }

                assert!(result as usize == available);
                self.events.set_len(available);
            };

            // dispatch the retrieved events to the associated futures
            for ref event in &self.events {
                let request_state: &mut RequestState =
                    unsafe { mem::transmute(event.data as usize) };
                request_state
                    .completed_sender
                    .take()
                    .unwrap()
                    .send(event.res)
                    .unwrap();
            }
        }
    }
}
