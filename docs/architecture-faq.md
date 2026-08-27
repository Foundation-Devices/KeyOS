# KeyOS architecture FAQ

Some architectural decisions are hidden deep in low-level code, guaranteed by
hardware, or were accepted as limitations. Most of them exist to keep the code
small, because less code means fewer bugs.

None of them are obvious from the code that depends on them, so they are
collected here and you don't have to re-read the kernel for every change.

Expand the list when something gets flagged for the third time and has to be
explained away for the third time.

## Target hardware

The device is a 500MHz ARM with 128MB of RAM, running a kernel with user mode
processes and a mostly complete `std`, heap included. It is not a
microcontroller, so it does not need the microoptimizations you would write for
an ESP32.

It runs on a battery, but there is no CPU throttling, so fewer cycles do not
mean less power. Idle time does, so prefer event-driven designs over polling or
other constant background work.

## Hosted mode

Hosted builds swap the device drivers for mocks so the system runs on a desktop.
Those mocks are exempt from the security requirements, because nothing real sits
behind them.

## Memory

Physical RAM is encrypted, so a readout of the DRAM chip is not a concern.
Allocations opt out with `MemoryFlags::PLAINTEXT`, which exists for peripherals
whose DMA cannot reach encrypted pages (LCDC, ISC, SDMMC).

Memory spaces are per process, so one process cannot read another's secrets.

Freed pages are scrubbed to zero pretty much immediately, and only scrubbed
pages are handed out on allocation. Process exit sanitizes everything. Code
therefore does not need to zeroize session keys, passwords, KDF output, or
derived keys. The kernel's memory manager and its page zeroer do the work.

Copies of secrets left behind inside a live process are an accepted limitation.
Zeroization is a lot of work for little benefit in practice.

The device seed, the app seed (`GetSeed`, `GetAppSeed`), and asymmetric private
keys are zeroized explicitly regardless. The seed is in a class of its own:
nothing else on the device is that sensitive.

## Logging

`log::trace!` may carry hexdumps of messages and internal structures, sensitive
ones included. No service ships with trace as its default level, so those dumps
only exist on a build someone is actively debugging.

## Panics

Panics are immediate aborts and nothing unwinds, so mutex poisoning, double
panics, and destructors running on the way out are not concerns. Locks can be
unwrapped every time. The strategy comes from the `armv7a-unknown-xous-elf`
toolchain and the `panic-immediate-abort` cargo feature.

## System services

`xtask` holds the list of system services started during boot, and if any of
them panics the kernel panics with it. They can therefore be considered always
running and the servers they expose always available, so connecting to them and
messaging them does not need the fallible APIs.

## Interrupts

Claiming an interrupt is a privileged syscall, and in practice nothing ever
frees one. Handlers all live in system processes, which cannot die without
taking the kernel down with them.

## IPC and permissions

Messaging permissions are declared in manifests and enforced at runtime by
`app-manager`, `xous-names`, and the kernel working in tandem. The
`MessageAllowed<M>` bounds that `use_api!` generates are developer quality of
life, turning a missing grant into a compile error rather than a runtime
failure. They are not the enforcement.

A server that receives a message has therefore already had its caller vetted and
does not need to check who sent it. The check covers neither of two other
things: a server with several legitimate callers still has to tell them apart by
capability, and the message contents are unvalidated either way.

The manifest fields behind those grants (`permissionGroup`, `requiredSignature`,
`approval`) interact in a way none of them shows on its own, and `autoAllow` in
particular does not mean any app may send the message. Read `ApprovalBehavior`
in `os/app-manifest` before taking a policy off a manifest.

## App IDs

App manifests are either signed and vetted by Foundation, or installed with a
certificate imported in Developer Mode, which waives most security guarantees by
design. So `app_id` and the manifest declarations can be trusted, as much as
`app-manager` trusts them. Builtin and sideloaded apps differ here; check
`app-manager` when it matters.

## Filesystem operations

Three classes of filesystem reliability, keyed by the `Location` enum in the
filesystem API:

- **System partition** (`System`, `CommonAssets`, `SystemAppData`,
  `AppResources`): always available from boot.
  It has no intermittent failures, so most operations need neither to be
  fallible nor to roll back. A file you could open and write will still be there
  to read, unless someone else deleted it in the meantime. It can only fail
  through eMMC failure (which panics the system) or a corrupted filesystem, a
  can of worms we don't tackle here.
- **Encrypted partition** (`AppData`, `EncryptedRoot`, `User`): available from
  the first successful unlock until poweroff. Once available it has the same
  guarantees as system.
- **USB and Airlock** (`Usb`, `Airlock`): these can appear and disappear at any
  time, and operations can fail as such. Not intermittent per se, but handling
  files on these partitions is a challenge.

`Boot` is the boot volume, reserved for firmware upgrade and recovery, and does
not fall into any of the three.

Infallible is not the same as atomic. Power loss is still real, so the order of
operations matters: a sequence must not leave state the next boot misreads, and
a destructive step belongs after everything that can still abort. What system
infallibility removes is error-path recovery and taxonomies of why a write
failed, not the need to think about ordering.

Closing a file flushes it, as do directory operations like rename and remove, so
an explicit flush only matters while a file stays open.
