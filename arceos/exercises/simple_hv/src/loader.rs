use std::io::{self, Read};
use std::fs::File;
use axhal::paging::MappingFlags;
use axhal::mem::{PAGE_SIZE_4K, phys_to_virt};
use axmm::AddrSpace;
use crate::VM_ENTRY;

/// The hart id value returned to the guest when it reads `mhartid`.
pub const GUEST_HARTID: usize = 0x1234;

/// The magic value the guest expects to read from guest-physical 0x40.
pub const MAGIC_VALUE: u64 = 0x6688;

/// The guest-physical address where the magic value is placed.
pub const MAGIC_ADDR: usize = 0x40;

/// Guest-physical address range flags for normal memory.
const RWU: MappingFlags = MappingFlags::READ.union(MappingFlags::WRITE).union(MappingFlags::USER);

pub fn load_vm_image(fname: &str, uspace: &mut AddrSpace) -> io::Result<()> {
    let mut buf = [0u8; PAGE_SIZE_4K];
    let n = load_file(fname, &mut buf)?;
    ax_println!("guest image size: {n} bytes");

    let code_flags =
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE | MappingFlags::USER;

    // Map the guest code page at the guest entry and copy the image in.
    uspace
        .map_alloc(VM_ENTRY.into(), PAGE_SIZE_4K, code_flags, true)
        .unwrap();
    write_guest_phys(uspace, VM_ENTRY, &buf[..n]);

    // Map a scratch page at guest-physical 0x0 and place the magic value at
    // 0x40. The guest loads its `a0` from there: `ld a0, 64(zero)`.
    uspace
        .map_alloc(0x0.into(), PAGE_SIZE_4K, RWU, true)
        .unwrap();
    write_guest_phys(uspace, MAGIC_ADDR, &MAGIC_VALUE.to_ne_bytes());

    Ok(())
}

/// Writes `data` into guest-physical memory at `gpa` through the EPT mapping.
fn write_guest_phys(uspace: &AddrSpace, gpa: usize, data: &[u8]) {
    let (paddr, _, _) = uspace
        .page_table()
        .query(gpa.into())
        .unwrap_or_else(|_| panic!("Mapping failed for gpa: {gpa:#x}"));
    ax_println!("write_guest_phys: gpa={gpa:#x} -> paddr={paddr:#x}");
    unsafe {
        core::ptr::copy_nonoverlapping(
            data.as_ptr(),
            phys_to_virt(paddr).as_mut_ptr(),
            data.len(),
        );
    }
}

fn load_file(fname: &str, buf: &mut [u8]) -> io::Result<usize> {
    ax_println!("app: {fname}");
    let mut file = File::open(fname)?;
    let n = file.read(buf)?;
    Ok(n)
}
