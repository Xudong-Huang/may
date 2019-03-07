#![allow(dead_code)]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

include!(concat!(env!("OUT_DIR"), "/linux_bindings.rs"));

/*
 * extracted from https://elixir.bootlin.com/linux/latest/source/include/uapi/linux/fs.h#L372
 * Flags for preadv2/pwritev2:
 */

/* per-IO O_DSYNC */
//#define RWF_DSYNC	((__force __kernel_rwf_t)0x00000002)
pub const RWF_DSYNC: u32 = 2;

/* per-IO O_SYNC */
//#define RWF_SYNC	((__force __kernel_rwf_t)0x00000004)
pub const RWF_SYNC: u32 = 4;

/* per-IO, return -EAGAIN if operation would block */
//#define RWF_NOWAIT	((__force __kernel_rwf_t)0x00000008)
pub const RWF_NOWAIT: u32 = 8;

pub use libc::c_long;

// Relevant symbols from the native bindings exposed via aio-bindings
// pub use self::{aio_context_t, io_event, iocb, syscall, timespec,
//                __NR_io_destroy, __NR_io_getevents, __NR_io_setup, __NR_io_submit,
//                IOCB_CMD_PREAD, IOCB_CMD_PWRITE, IOCB_CMD_FSYNC, IOCB_CMD_FDSYNC, IOCB_FLAG_RESFD,
//                RWF_DSYNC, RWF_SYNC};

// -----------------------------------------------------------------------------------------------
// Inline functions that wrap the kernel calls for the entry points corresponding to Linux
// AIO functions
// -----------------------------------------------------------------------------------------------

// Initialize an AIO context for a given submission queue size within the kernel.
//
// See [io_setup(7)](http://man7.org/linux/man-pages/man2/io_setup.2.html) for details.
#[inline(always)]
pub unsafe fn io_setup(nr: c_long, ctxp: *mut aio_context_t) -> c_long {
    syscall(__NR_io_setup as c_long, nr, ctxp)
}

// Destroy an AIO context.
//
// See [io_destroy(7)](http://man7.org/linux/man-pages/man2/io_destroy.2.html) for details.
#[inline(always)]
pub unsafe fn io_destroy(ctx: aio_context_t) -> c_long {
    syscall(__NR_io_destroy as c_long, ctx)
}

// Submit a batch of IO operations.
//
// See [io_sumit(7)](http://man7.org/linux/man-pages/man2/io_submit.2.html) for details.
#[inline(always)]
pub unsafe fn io_submit(ctx: aio_context_t, nr: c_long, iocbpp: *mut *mut iocb) -> c_long {
    syscall(__NR_io_submit as c_long, ctx, nr, iocbpp)
}

// Retrieve completion events for previously submitted IO requests.
//
// See [io_getevents(7)](http://man7.org/linux/man-pages/man2/io_getevents.2.html) for details.
#[inline(always)]
pub unsafe fn io_getevents(
    ctx: aio_context_t,
    min_nr: c_long,
    max_nr: c_long,
    events: *mut io_event,
    timeout: *mut timespec,
) -> c_long {
    syscall(
        __NR_io_getevents as c_long,
        ctx,
        min_nr,
        max_nr,
        events,
        timeout,
    )
}
