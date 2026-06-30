/*
 * SPDX-FileCopyrightText: 2022 Foundation Devices, Inc <hello@foundation.xyz>
 * SPDX-License-Identifier: Apache-2.0
 */

.section .text.reset_vector, "ax"

.global reset
.align 4
.arm

reset:

_exception_vectors_kernel:
  b   reset_vector          /* Entrypoint */
  b   undef_handler         /* Undefined Instruction */
  b   svc_handler           /* Supervisor Call (used in syscalls) */
  b   abort_handler         /* Prefetch Abort */
  b   abort_handler         /* Data Abort */
.word  0 /* Empty */
  b   irq_handler           /* IRQ */
  b   fiq_handler           /* FIQ */

fiq_handler:
  bkpt /* FIQs aren't supported yet */
  b fiq_handler

reset_vector:

/* Branch to the kernel init and then to the kernel entrypoint */
_branch_main:
    # Configure VBAR to point to the kernel's interrupt vector table defined above
    mrc p15, 0, r4, c12, c0, 0 /* Read VBAR into r4 */
    ldr r4, =_exception_vectors_kernel
    mcr p15, 0, r4, c12, c0, 0

    /* Clear all faults if any */
    mov ip, #0
    mcr p15, 0, ip, c6, c0, 0
    mcr p15, 0, ip, c6, c0, 2
    mcr p15, 0, ip, c5, c0, 0
    mcr p15, 0, ip, c5, c0, 1

    /* Save received stack pointer for later use */
    mov r4, sp

    /* Configure exception mode stack pointers */
    msr cpsr_c, #0x12
    ldr sp, =({KERNEL_MODE_STACK_TOP})
    msr cpsr_c, #0x11
    ldr sp, =({KERNEL_MODE_STACK_TOP})
    msr cpsr_c, #0x17
    ldr sp, =({KERNEL_MODE_STACK_TOP})
    msr cpsr_c, #0x1b
    ldr sp, =({KERNEL_MODE_STACK_TOP})

    msr cpsr_c, #(0x1f | 0x80 | 0x40) /* Switch to System mode (IRQs disabled) */

    /* Set System mode stack */
    mov sp, r4

    /* Note that registers r0-r3 still contain the kernel arguments */
    ldr lr, =(init)
    blx lr
    b .

/*
    Exception handlers.

    Each one saves process context into the Thread slot pointed to by
    TPIDRPRW and continues in its Rust counterpart on the kernel stack.
    Syscalls skip the FPU state: the syscall ABI clobbers it (see the
    xous-rs syscall stub), so the resume path zeroes it instead of
    restoring it. Return happens elsewhere.
*/
.macro save_context fp_regs
    mrc p15, 0, sp, c13, c0, 4  /* TPIDRPRW holds the current thread's context address */
    stm sp, {{r0 - r14}}^   /* 0-14: Saved user registers */
    add sp, #(4 * 15)
    stm sp!, {{lr}}         /* 15: This context's LR is the PC of the original one */

    # Special registers
    mrs r1, spsr            /* 16: Saved PSR */
    mrc p15, 0, r2, c13, c0, 2  /* 17: Hardware thread pointer register */
    vmrs r3, fpscr          /* 18: Floating point control reg */
    stm sp!, {{r1 - r3}}    /* Save special registers read above */

    mov r1, #\fp_regs
    str r1, [sp], #4        /* 19: fp_regs_saved */
.if \fp_regs
    vstm sp!, {{d0 - d15}}  /* 20-51: FPU registers */
    vstm sp!, {{d16 - d31}} /* 52-83: FPU registers */
.endif

    ldr sp, =({KERNEL_MODE_STACK_TOP})
.endm

undef_handler:
    save_context fp_regs=1
    blx _undef_handler_rust

svc_handler:
    save_context fp_regs=0
    blx _swi_handler_rust

abort_handler:
    save_context fp_regs=1
    blx _abort_handler_rust

irq_handler:
    save_context fp_regs=1
    blx _irq_handler_rust

/*
   A thread resume routine.
*/
.align 4
.thumb
.global _resume_trampoline
_resume_trampoline:
    bx pc
    nop
    .arm
    add ip, r0, #(4 * 15)
    ldm ip!, {{lr}}             /* 15: Resume address */
    ldm ip!, {{r1-r3}}          /* Load special registers in one go*/
    msr spsr_cxsf, r1           /* 16: Saved PSR */
    mcr p15, 0, r2, c13, c0, 2  /* 17: Hardware thread pointer register */
    vmsr    fpscr, r3           /* 18: Floating point control */

    ldm ip!, {{r1}}             /* 19: fp_regs_saved */
    cmp r1, #0
    bne _resume_restore_fp_regs

    /* No FPU state in the frame. The regs are ABI-clobbered, but kernel
       NEON may have left secrets in them, so zero instead of restore. */
.irp reg, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
    veor q\reg, q\reg, q\reg
.endr

_resume_to_thread:
    ldm r0, {{r0 - r14}}^       /* 0-14: Saved user registers */
    movs pc, lr

_resume_restore_fp_regs:
    vldm    ip!, {{d0 - d15}}   /* 20-51: FPU registers */
    vldm    ip!, {{d16 - d31}}  /* 52-83: FPU registers */
    b _resume_to_thread

.global __aeabi_unwind_cpp_pr0
__aeabi_unwind_cpp_pr0:
 b .

