#![cfg_attr(feature = "axstd", no_std)]
#![cfg_attr(feature = "axstd", no_main)]
#![feature(asm_const)]
#![feature(riscv_ext_intrinsics)]

#[cfg(feature = "axstd")]
extern crate axstd as std;
extern crate alloc;
#[macro_use]
extern crate axlog;

mod task;
mod vcpu;
mod regs;
mod csrs;
mod sbi;
mod loader;

use vcpu::VmCpuRegisters;
use riscv::register::{scause, sstatus, stval};
use csrs::defs::hstatus;
use tock_registers::LocalRegisterCopy;
use csrs::{RiscvCsrTrait, CSR};
use vcpu::_run_guest;
use sbi::SbiMessage;
use loader::{load_vm_image, GUEST_HARTID};
use axhal::mem::PhysAddr;
use crate::regs::GprIndex::{self, A0, A1};

const VM_ENTRY: usize = 0x8020_0000;

/// Exception code of the virtual instruction exception per the RISC-V
/// privilege specification (guest accesses to M-mode CSRs from VS-mode).
const EXC_VIRTUAL_INSTRUCTION: usize = 20;

/// CSR address of `mhartid`.
const CSR_MHARTID: usize = 0xf14;

#[cfg_attr(feature = "axstd", no_mangle)]
fn main() {
    ax_println!("Hypervisor ...");

    // A new address space for vm.
    let mut uspace = axmm::new_user_aspace().unwrap();

    // Load vm binary file into address space.
    if let Err(e) = load_vm_image("/sbin/skernel2", &mut uspace) {
        panic!("Cannot load app! {:?}", e);
    }

    // Setup context to prepare to enter guest mode.
    let mut ctx = VmCpuRegisters::default();
    prepare_guest_context(&mut ctx);

    // Setup pagetable for 2nd address mapping.
    let ept_root = uspace.page_table_root();
    prepare_vm_pgtable(ept_root);

    // Kick off vm and wait for it to exit.
    while !run_guest(&mut ctx) {
    }

    panic!("Hypervisor ok!");
}

fn prepare_vm_pgtable(ept_root: PhysAddr) {
    let hgatp = 8usize << 60 | usize::from(ept_root) >> 12;
    unsafe {
        core::arch::asm!(
            "csrw hgatp, {hgatp}",
            hgatp = in(reg) hgatp,
        );
        core::arch::riscv64::hfence_gvma_all();
    }
}

fn run_guest(ctx: &mut VmCpuRegisters) -> bool {
    unsafe {
        _run_guest(ctx);
    }

    vmexit_handler(ctx)
}

/// Reads the `htinst` CSR, which contains the trapped instruction encoding
/// (or a standard pseudo-encoding) on a guest trap.
fn read_htinst() -> usize {
    let v: usize;
    unsafe {
        core::arch::asm!("csrr {}, htinst", out(reg) v);
    }
    v
}

/// Emulates a guest `csrr rd, mhartid` trap: writes the emulated hart id
/// into the destination register and advances `sepc` past the instruction.
fn emulate_csrr_mhartid(ctx: &mut VmCpuRegisters, htinst: usize) {
    if htinst == 0 {
        // No instruction encoding available; the only CSR access the guest
        // performs is `csrr a1, mhartid`.
        ctx.guest_regs.gprs.set_reg(A1, GUEST_HARTID);
    } else {
        let csr = (htinst >> 20) & 0xfff;
        let rd = ((htinst >> 7) & 0x1f) as u32;
        if csr != CSR_MHARTID {
            panic!("Unsupported guest CSR access: {csr:#x} (htinst: {htinst:#x})");
        }
        if let Some(reg) = GprIndex::from_raw(rd) {
            ctx.guest_regs.gprs.set_reg(reg, GUEST_HARTID);
        } else {
            panic!("Bad destination register for CSR access: {rd}");
        }
    }
    ctx.guest_regs.sepc += 4;
}

#[allow(unreachable_code)]
fn vmexit_handler(ctx: &mut VmCpuRegisters) -> bool {
    use scause::{Exception, Trap};

    let scause = scause::read();
    match scause.cause() {
        Trap::Exception(Exception::VirtualSupervisorEnvCall) => {
            let sbi_msg = SbiMessage::from_regs(ctx.guest_regs.gprs.a_regs()).ok();
            ax_println!("VmExit Reason: VSuperEcall: {:?}", sbi_msg);
            if let Some(msg) = sbi_msg {
                match msg {
                    SbiMessage::Reset(_) => {
                        let a0 = ctx.guest_regs.gprs.reg(A0);
                        let a1 = ctx.guest_regs.gprs.reg(A1);
                        ax_println!("a0 = {:#x}, a1 = {:#x}", a0, a1);
                        assert_eq!(a0, 0x6688);
                        assert_eq!(a1, 0x1234);
                        ax_println!("Shutdown vm normally!");
                        return true;
                    },
                    _ => todo!(),
                }
            } else {
                panic!("bad sbi message! ");
            }
        },
        Trap::Exception(Exception::InstructionGuestPageFault) => {
            // NOTE: the rcore-os riscv crate maps exception code 20 to
            // `InstructionGuestPageFault`, but code 20 is the *virtual
            // instruction* exception per the RISC-V privilege spec. The
            // guest's `csrr a1, mhartid` traps here (accessing an M-mode
            // CSR from VS-mode). Emulate the CSR read and resume.
            let htinst = read_htinst();
            ax_println!(
                "VmExit Reason: VirtualInstruction (sepc: {:#x}, htinst: {htinst:#x})",
                ctx.guest_regs.sepc
            );
            emulate_csrr_mhartid(ctx, htinst);
        },
        Trap::Exception(Exception::IllegalInstruction) => {
            // QEMU reports an illegal instruction (not a virtual
            // instruction exception) when the guest reads `mhartid` from
            // VS-mode. `stval` holds the trapping instruction encoding, so
            // we can identify `csrr rd, mhartid` and emulate it.
            let ins = stval::read();
            let csr = (ins >> 20) & 0xfff;
            let funct3 = (ins >> 12) & 0b111;
            let opcode = ins & 0x7f;
            if opcode == 0x73 && funct3 == 2 && csr == CSR_MHARTID {
                ax_println!("VmExit Reason: IllegalInstruction (guest csrr mhartid), emulated");
                let rd = ((ins >> 7) & 0x1f) as u32;
                if let Some(reg) = GprIndex::from_raw(rd) {
                    ctx.guest_regs.gprs.set_reg(reg, GUEST_HARTID);
                }
                ctx.guest_regs.sepc += 4;
            } else {
                panic!("Bad instruction: {:#x} sepc: {:#x}", ins, ctx.guest_regs.sepc);
            }
        },
        Trap::Exception(Exception::LoadGuestPageFault) => {
            panic!("LoadGuestPageFault: stval{:#x} sepc: {:#x}",
                stval::read(),
                ctx.guest_regs.sepc
            );
        },
        Trap::Interrupt(_) => {
            // Interrupts are disabled while the guest runs, but just in
            // case one slips through, resume the guest without servicing.
            ax_println!("VmExit Reason: interrupt ignored");
        },
        _ => {
            panic!(
                "Unhandled trap: {:?}, sepc: {:#x}, stval: {:#x}",
                scause.cause(),
                ctx.guest_regs.sepc,
                stval::read()
            );
        }
    }
    false
}

fn prepare_guest_context(ctx: &mut VmCpuRegisters) {
    // Set hstatus
    let mut hstatus = LocalRegisterCopy::<usize, hstatus::Register>::new(
        riscv::register::hstatus::read().bits(),
    );
    // Set Guest bit in order to return to guest mode.
    hstatus.modify(hstatus::spv::Guest);
    // Set SPVP bit in order to accessing VS-mode memory from HS-mode.
    hstatus.modify(hstatus::spvp::Supervisor);
    CSR.hstatus.write_value(hstatus.get());
    ctx.guest_regs.hstatus = hstatus.get();

    // Set sstatus in guest mode.
    let mut sstatus = sstatus::read();
    sstatus.set_spp(sstatus::SPP::Supervisor);
    // Disable HS-level interrupts while running the guest: the guest only
    // executes a handful of instructions and returns immediately. Otherwise
    // a timer interrupt would trap into our VM-exit handler, which does not
    // (and must not) service ArceOS scheduler preemption.
    sstatus.set_sie(false);
    ctx.guest_regs.sstatus = sstatus.bits();
    // Return to entry to start vm.
    ctx.guest_regs.sepc = VM_ENTRY;
}
