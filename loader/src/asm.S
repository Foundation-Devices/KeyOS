/*
 * SPDX-FileCopyrightText: 2023 Foundation Devices, Inc <hello@foundation.xyz>
 * SPDX-License-Identifier: Apache-2.0
 */

.section .text.init, "ax"
.text

.global reset
.align 4
.arm
reset:

/* Exception vectors (should be a branch to be detected as a valid code by the rom */
_exception_vectors:
  b   reset_vector    /* reset */
  b   undef_vector   /* Undefined Instruction */
  b   swi_vector     /* Software Interrupt */
  b   pabt_vector    /* Prefetch Abort */
  b   dabt_vector    /* Data Abort */
.word  0 /* Empty */
  b   irq_vector  /* IRQ : read the AIC */
  b   fiq_vector      /* FIQ */

undef_vector:
  bkpt /* Issue debugger breakpoint */
  b   undef_vector
swi_vector:
  bkpt /* Issue debugger breakpoint */
  b   swi_vector
pabt_vector:
  bkpt /* Issue debugger breakpoint */
  b   pabt_vector
dabt_vector:
  bkpt /* Issue debugger breakpoint */
  b dabt_vector
rsvd_vector:
  bkpt /* Issue debugger breakpoint */
  b   rsvd_vector
irq_vector:
  bkpt /* Issue debugger breakpoint */
  b   irq_vector
fiq_vector:
  bkpt /* Issue debugger breakpoint */
  b   fiq_vector
reset_vector:

/*
 * We must configure VBAR for the reset vectors to be at
 * the start of memory (_mem_start)
 */
mrc p15, 0, r2, c12, c0, 0 /* Read VBAR into R2 */
ldr  r2, =_mem_start
mcr  p15, 0, r2, c12, c0, 0

/* Enable FPU */
/* Grant non secure access for CP10 and CP11 */
mrc  p15, 0, r0, c1, c1, 2
orr  r0, r0, #3 << 10
mcr  p15, 0, r0, c1, c1, 2
/* Set CP10 and CP11 access permission (Privileged and User mode) */
ldr  r0, =(0xF << 20)
mcr  p15, 0, r0, c1, c0, 2
/* Set the FPEXC EN bit to enable the FPU (and NEON instructions) */
ldr r1, =(0x40000000)
vmsr FPEXC, r1

/* Init the stack */
_init_stack:

ldr r0, =_ebss

# Set the stack for IRQ mode
msr cpsr_c, #0xd2
mov sp, r0

# Set the stack for FIQ mode
msr cpsr_c, #0xd1
mov sp, r0

# Set the stack for Abort mode
msr cpsr_c, #0xd7
mov sp, r0

# Set the stack for Undefined Instruction mode
msr cpsr_c, #0xdb
mov sp, r0

# Back to Supervisor mode, (IRQ and FIQ both masked, Arm instruction
# set) set the stack for Supervisor mode
msr cpsr_c, #0xd3
mov sp, r0

/* Clear Abort condition if it is pending with help of the abort handler */
ldr  r1, =(0x100)
mrs  r0, cpsr
eor  r0, r0, r1
msr  cpsr_x, r0
nop
eor  r0, r0, r1
msr  cpsr_x, r0

/* Jump to the entrypoint */
_branch_loader_main:
    # Loader arguments: kernel image address, signature
    ldr r0, =(_ebss)
    mov r1, #0
    blx rust_entry
    b .

.thumb
.global start_kernel
start_kernel:
  # Set TTBR0 and TTBR1 to point to the kernel page table stored in arg #2 (r1).
  # Mark the translation-table walks inner+outer Write-Back Write-Allocate,
  # Non-shareable (IRGN=WB-WA bit6, RGN=WB-WA bit3, S=0). Non-shareable is
  # required: with ACTLR.SMP=0 the A5 treats shareable WB-WA as Non-cacheable.
  orr r1, r1, #0x48
  mcr p15, 0, r1, c2, c0, 0
  mcr p15, 0, r1, c2, c0, 1

  # Set TTBCR to split TTBR0 and TTBR1 at 0x80000000
  ldr r12, =(0x01)
  mcr p15, 0, r12, c2, c0, 2

  # Set every domain in DACR to "Client" to enable permission checks
  ldr r12, =(0x55555555)
  mcr p15, 0, r12, c3, c0, 0 /* Set DACR */

  # Configure TRE remapping registers
  # PRRR: mostly transparent config, B means device, C means cacheable, TEX[0] unused
  # NS1: 1, NS0: 0, DS1: 1, D0: 1
  # TR0 (C:0, B:0): 10 (Normal)
  # TR1 (C:0, B:1): 01 (Device)
  # TR2 (C:1, B:0): 10 (Normal)
  # TR3 (C:1, B:1): 01 (Device)
  # TR4-7: all 0 (strongly ordered)
  ldr r12, =(0xb0066)
  mcr p15, 0, r12, c10, c2, 0

  # NMRR:
  # IR0 (C:0, B:0): 00 (Non-cacheable)
  # IR1 (C:0, B:1): 00 (Non-cacheable)
  # IR2 (C:1, B:0): 01 (Write-Back, Write-Allocate.)
  # IR3 (C:1, B:1): 00 (Non-cacheable)
  # IR4-7: all non-cacheable
  # OR*: Same as IR*
  ldr r12, =(0x00100010)
  mcr p15, 0, r12, c10, c2, 1

  # Set ASID and PID
  ldr r12, =(0x00000101)
  mcr p15, 0, r12, c13, c0, 1

  # Configure the system
  mrc p15, 0, r12, c1, c0, 0
  orr r12, #0x1 /* MMU enable */
  orr r12, #0x10000000 /* TRE=1 */
  mcr p15, 0, r12, c1, c0, 0
  nop
  nop
  nop

  # Prevent speculative access after MMU is enabled
  isb
  dsb

  # Flush caches and branch predictor
  mcr p15, 0, r12, c7, c5, 0
  mcr p15, 0, r12, c7, c5, 6
  isb
  dsb

_prep_args:

  # Set kernel stack pointer
  mov sp, r0

  # Prepare arguments for the kernel `init` function (entrypoint)
  mov r0, r3

  # Jump to kernel
  bx r2

.global __aeabi_unwind_cpp_pr0
__aeabi_unwind_cpp_pr0:
  b __aeabi_unwind_cpp_pr0
