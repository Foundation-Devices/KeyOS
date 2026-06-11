use core::arch::asm;

use crate::definitions::SysCallResult;
use crate::syscall::SysCall;

#[inline]
pub fn syscall(call: SysCall) -> SysCallResult {
    let [mut a0, mut a1, mut a2, mut a3, mut a4, mut a5, mut a6, mut a7] = call.as_args();

    unsafe {
        asm!(
            "svc #0",
            inout("r0") a0,
            inout("r1") a1,
            inout("r2") a2,
            inout("r3") a3,
            inout("r4") a4,
            inout("r5") a5,
            // R6 and R7 are used by LLVM internally
            inout("r8") a6,
            inout("r9") a7,
            // The kernel does not preserve VFP state across a syscall.
            lateout("d0") _, lateout("d1") _, lateout("d2") _, lateout("d3") _,
            lateout("d4") _, lateout("d5") _, lateout("d6") _, lateout("d7") _,
            lateout("d8") _, lateout("d9") _, lateout("d10") _, lateout("d11") _,
            lateout("d12") _, lateout("d13") _, lateout("d14") _, lateout("d15") _,
            lateout("d16") _, lateout("d17") _, lateout("d18") _, lateout("d19") _,
            lateout("d20") _, lateout("d21") _, lateout("d22") _, lateout("d23") _,
            lateout("d24") _, lateout("d25") _, lateout("d26") _, lateout("d27") _,
            lateout("d28") _, lateout("d29") _, lateout("d30") _, lateout("d31") _,
            options(nostack)
        );
    };

    let ret = crate::Result::from_args([a0, a1, a2, a3, a4, a5, a6, a7]);
    match ret {
        crate::Result::Error(e) => Err(e),
        other => Ok(other),
    }
}
