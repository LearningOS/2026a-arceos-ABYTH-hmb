#![allow(dead_code)]

use core::ffi::{c_void, c_char, c_int};
use axhal::arch::TrapFrame;
use axhal::trap::{register_trap_handler, SYSCALL};
use axhal::mem::PAGE_SIZE_4K;
use axhal::paging::MappingFlags;
use axerrno::LinuxError;
use axtask::current;
use axtask::TaskExtRef;
use memory_addr::{VirtAddr, VirtAddrRange};
use arceos_posix_api as api;

const SYS_IOCTL: usize = 29;
const SYS_OPENAT: usize = 56;
const SYS_CLOSE: usize = 57;
const SYS_READ: usize = 63;
const SYS_WRITE: usize = 64;
const SYS_WRITEV: usize = 66;
const SYS_EXIT: usize = 93;
const SYS_EXIT_GROUP: usize = 94;
const SYS_SET_TID_ADDRESS: usize = 96;
const SYS_MMAP: usize = 222;

const AT_FDCWD: i32 = -100;

/// Macro to generate syscall body
///
/// It will receive a function which return Result<_, LinuxError> and convert it to
/// the type which is specified by the caller.
#[macro_export]
macro_rules! syscall_body {
    ($fn: ident, $($stmt: tt)*) => {{
        #[allow(clippy::redundant_closure_call)]
        let res = (|| -> axerrno::LinuxResult<_> { $($stmt)* })();
        match res {
            Ok(_) | Err(axerrno::LinuxError::EAGAIN) => debug!(concat!(stringify!($fn), " => {:?}"),  res),
            Err(_) => info!(concat!(stringify!($fn), " => {:?}"), res),
        }
        match res {
            Ok(v) => v as _,
            Err(e) => {
                -e.code() as _
            }
        }
    }};
}

bitflags::bitflags! {
    #[derive(Debug)]
    /// permissions for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    struct MmapProt: i32 {
        /// Page can be read.
        const PROT_READ = 1 << 0;
        /// Page can be written.
        const PROT_WRITE = 1 << 1;
        /// Page can be executed.
        const PROT_EXEC = 1 << 2;
    }
}

impl From<MmapProt> for MappingFlags {
    fn from(value: MmapProt) -> Self {
        let mut flags = MappingFlags::USER;
        if value.contains(MmapProt::PROT_READ) {
            flags |= MappingFlags::READ;
        }
        if value.contains(MmapProt::PROT_WRITE) {
            flags |= MappingFlags::WRITE;
        }
        if value.contains(MmapProt::PROT_EXEC) {
            flags |= MappingFlags::EXECUTE;
        }
        flags
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    /// flags for sys_mmap
    ///
    /// See <https://github.com/bminor/glibc/blob/master/bits/mman.h>
    struct MmapFlags: i32 {
        /// Share changes
        const MAP_SHARED = 1 << 0;
        /// Changes private; copy pages on write.
        const MAP_PRIVATE = 1 << 1;
        /// Map address must be exactly as requested, no matter whether it is available.
        const MAP_FIXED = 1 << 4;
        /// Don't use a file.
        const MAP_ANONYMOUS = 1 << 5;
        /// Don't check for reservations.
        const MAP_NORESERVE = 1 << 14;
        /// Allocation is for a stack.
        const MAP_STACK = 0x20000;
    }
}

#[register_trap_handler(SYSCALL)]
fn handle_syscall(tf: &TrapFrame, syscall_num: usize) -> isize {
    ax_println!("handle_syscall [{}] ...", syscall_num);
    let ret = match syscall_num {
         SYS_IOCTL => sys_ioctl(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _) as _,
        SYS_SET_TID_ADDRESS => sys_set_tid_address(tf.arg0() as _),
        SYS_OPENAT => sys_openat(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _, tf.arg3() as _),
        SYS_CLOSE => sys_close(tf.arg0() as _),
        SYS_READ => sys_read(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_WRITE => sys_write(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_WRITEV => sys_writev(tf.arg0() as _, tf.arg1() as _, tf.arg2() as _),
        SYS_EXIT_GROUP => {
            ax_println!("[SYS_EXIT_GROUP]: system is exiting ..");
            axtask::exit(tf.arg0() as _)
        },
        SYS_EXIT => {
            ax_println!("[SYS_EXIT]: system is exiting ..");
            axtask::exit(tf.arg0() as _)
        },
        SYS_MMAP => sys_mmap(
            tf.arg0() as _,
            tf.arg1() as _,
            tf.arg2() as _,
            tf.arg3() as _,
            tf.arg4() as _,
            tf.arg5() as _,
        ),
        _ => {
            ax_println!("Unimplemented syscall: {}", syscall_num);
            -LinuxError::ENOSYS.code() as _
        }
    };
    ret
}

/// `mmap` syscall: map the given file (or anonymous memory) into the current
/// user address space and return the mapped virtual address.
fn sys_mmap(
    addr: *mut usize,
    length: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    _offset: isize,
) -> isize {
    if length == 0 {
        return -LinuxError::EINVAL.code() as isize;
    }
    let prot_flags = MappingFlags::from(MmapProt::from_bits_truncate(prot));
    let map_flags = MmapFlags::from_bits_truncate(flags);
    let map_size = (length + PAGE_SIZE_4K - 1) & !(PAGE_SIZE_4K - 1);

    let curr = current();
    let mut aspace = curr.task_ext().aspace.lock();

    // Choose a virtual address for the new mapping. The user stack is mapped
    // right below the end of the address space, so keep away from it.
    let limit = VirtAddrRange::from_start_size(
        aspace.base(),
        aspace.end().as_usize() - crate::USER_STACK_SIZE - aspace.base().as_usize(),
    );
    let vaddr = if map_flags.contains(MmapFlags::MAP_FIXED) {
        VirtAddr::from(addr as usize)
    } else {
        let hint = if addr.is_null() {
            VirtAddr::from(0x400_0000)
        } else {
            VirtAddr::from(addr as usize)
        };
        match aspace.find_free_area(hint, map_size, limit) {
            Some(a) => a,
            None => return -LinuxError::ENOMEM.code() as isize,
        }
    };

    if aspace
        .map_alloc(
            vaddr,
            map_size,
            MappingFlags::USER | prot_flags,
            true, // populate: map and allocate all pages up front
        )
        .is_err()
    {
        return -LinuxError::ENOMEM.code() as isize;
    }

    // Fill in the contents: zero pages for anonymous mappings, or the file
    // contents read through the POSIX fd table for file-backed mappings.
    if map_flags.contains(MmapFlags::MAP_ANONYMOUS) || fd < 0 {
        let zeros = alloc::vec![0u8; map_size];
        if aspace.write(vaddr, &zeros).is_err() {
            return -LinuxError::EFAULT.code() as isize;
        }
    } else {
        let mut buf = alloc::vec![0u8; length];
        let n = api::sys_read(fd, buf.as_mut_ptr() as *mut c_void, length);
        if n < 0 {
            return n;
        }
        if n > 0 && aspace.write(vaddr, &buf[..n as usize]).is_err() {
            return -LinuxError::EFAULT.code() as isize;
        }
    }

    ax_println!(
        "sys_mmap: mapped [{:#x}, {:#x}) size={:#x} prot={:#b} flags={:#b} fd={}",
        vaddr.as_usize(),
        vaddr.as_usize() + map_size,
        map_size,
        prot,
        flags,
        fd
    );
    vaddr.as_usize() as isize
}

fn sys_openat(dfd: c_int, fname: *const c_char, flags: c_int, mode: api::ctypes::mode_t) -> isize {
    assert_eq!(dfd, AT_FDCWD);
    api::sys_open(fname, flags, mode) as isize
}

fn sys_close(fd: i32) -> isize {
    api::sys_close(fd) as isize
}

fn sys_read(fd: i32, buf: *mut c_void, count: usize) -> isize {
    api::sys_read(fd, buf, count)
}

fn sys_write(fd: i32, buf: *const c_void, count: usize) -> isize {
    api::sys_write(fd, buf, count)
}

fn sys_writev(fd: i32, iov: *const api::ctypes::iovec, iocnt: i32) -> isize {
    unsafe { api::sys_writev(fd, iov, iocnt) }
}

fn sys_set_tid_address(tid_ptd: *const i32) -> isize {
    let curr = current();
    curr.task_ext().set_clear_child_tid(tid_ptd as _);
    curr.id().as_u64() as isize
}

fn sys_ioctl(_fd: i32, _op: usize, _argp: *mut c_void) -> i32 {
    ax_println!("Ignore SYS_IOCTL");
    0
}
