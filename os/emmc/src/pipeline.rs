// SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{BTreeMap, VecDeque};

use xous::{DropDeallocate, MemoryFlags, MemoryRange};

use crate::{error::EmmcError, messages::*, BLOCK_SIZE, SD_BUFFER_BLOCKS};

/// Pool buffer size in bytes. All bounce buffers are allocated at the maximum DMA size so
/// the pool is a single uniform bucket and can satisfy any well-formed request.
const POOL_BUFFER_BYTES: usize = SD_BUFFER_BLOCKS * BLOCK_SIZE;

// Carrier types for caller responses. Production uses the framework deferreds; under test
// they're swapped for in-memory recorders so unit tests can drive the pipeline without a
// real Xous server.
#[cfg(not(test))]
type Deferred<M> = server::DeferredLendMut<M>;
#[cfg(not(test))]
type DeferredFlush = server::BlockingScalarResponse<()>;

#[cfg(test)]
type Deferred<M> = test_support::MockLendMut<M>;
#[cfg(test)]
type DeferredFlush = test_support::MockBlockingResponse<()>;

// XTS cluster size: a single AES-XTS encryption covers one cluster.
// A DMA transfer can span at most two clusters (if not cluster-aligned).
const XTS_CLUSTER_BLOCKS: usize = 64;

const PIPELINE_DEFER_THRESHOLD: usize = 8;

/// All side-effecting hardware/IPC interactions the pipeline performs. Production wires this
/// up to `KeyOsHardware`; tests use a recording mock.
pub trait HardwareImpl {
    /// Begin an SDMMC DMA on `block_index` for `blocks` blocks. `buf_ptr` is the page-aligned
    /// virt addr of the data buffer.
    fn sdmmc_dma(&mut self, direction: Direction, block_index: u32, blocks: usize, buf_ptr: *mut u8);

    /// Panic if the card has failed anything since the last call. Must not be called while a
    /// transfer is in flight, as it puts a command on the bus.
    fn assert_card_healthy(&mut self);

    /// Submit one disk-encrypt sub-op (one XTS cluster at most). Fire-and-forget; completion
    /// arrives via `Pipeline::on_disk_encrypt_complete`.
    fn disk_encrypt(
        &mut self,
        tweak: [u8; 16],
        j: usize,
        src: MemoryRange,
        dst: MemoryRange,
        dir: crypto::Direction,
    );

    /// Arm the suspend timer when the pipeline goes idle.
    fn idle(&mut self);

    /// Cancel any pending suspend timer when work arrives at a previously-idle pipeline.
    /// Prevents a stale timer from firing while a fresh op is in flight.
    fn cancel_idle(&mut self);

    /// Disable the SDMMC peripheral (called by the Suspend handler at idle).
    fn suspend_sdmmc(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockRange {
    pub start: u32,
    pub count: usize,
}

pub enum DmaBuf {
    Owned(DropDeallocate),
    #[allow(dead_code)]
    Borrowed(MemoryRange),
}

impl DmaBuf {
    fn as_mut_ptr(&self) -> *mut u8 {
        match self {
            DmaBuf::Owned(b) => b.as_mut_ptr(),
            DmaBuf::Borrowed(b) => b.as_mut_ptr(),
        }
    }

    #[allow(dead_code)]
    pub fn range(&self) -> MemoryRange {
        match self {
            DmaBuf::Owned(b) => **b,
            DmaBuf::Borrowed(b) => *b,
        }
    }
}

pub enum OperationType {
    PlainReadSdmmc {
        dma_buf: DmaBuf,
        deferred: Deferred<ReadBlocks>,
    },
    EncReadSdmmc {
        ciphertext: DropDeallocate,
        deferred: Deferred<ReadEncryptedBlocks>,
    },
    EncReadCrypt {
        ciphertext: DropDeallocate,
        crypt_offset: usize,
        deferred: Deferred<ReadEncryptedBlocks>,
    },
    PlainWriteSdmmc {
        owned_buf: DropDeallocate,
    },
    EncWriteCrypt {
        ciphertext: DropDeallocate,
        crypt_offset: usize,
        deferred: Deferred<WriteEncryptedBlocks>,
    },
    EncWriteSdmmc {
        ciphertext: DropDeallocate,
    },
    Flush {
        deferred: DeferredFlush,
    },
}

impl OperationType {
    fn is_sdmmc_step(&self) -> bool {
        matches!(
            self,
            OperationType::PlainReadSdmmc { .. }
                | OperationType::EncReadSdmmc { .. }
                | OperationType::PlainWriteSdmmc { .. }
                | OperationType::EncWriteSdmmc { .. }
        )
    }

    fn is_crypt_step(&self) -> bool {
        matches!(self, OperationType::EncReadCrypt { .. } | OperationType::EncWriteCrypt { .. })
    }

    fn is_flush(&self) -> bool { matches!(self, OperationType::Flush { .. }) }
}

/// Sentinel range for ops that participate in ordering as a barrier (Flush). Any other range
/// trivially overlaps it, so the seq+range gate naturally serializes work behind a flush.
pub const FULL_RANGE: BlockRange = BlockRange { start: 0, count: usize::MAX };

/// An admit-tagged op. The pipeline orders work by `seq` (admit order) and uses `range` for
/// overlap detection in the SDMMC scheduling gate.
struct Operation {
    seq: u64,
    range: BlockRange,
    op: OperationType,
}

pub struct Pipeline<H: HardwareImpl> {
    pub hw: H,
    /// Work admitted to the pipeline, keyed by admit-order seq. BTreeMap so iteration is naturally
    /// admit-ordered and gate lookups via `range(..seq)` are clean.
    ops: BTreeMap<u64, Operation>,
    sdmmc_running: Option<Operation>,
    crypt_running: Option<Operation>,
    /// Ops that arrived while the pipeline was at the back-pressure threshold. Drained in FIFO
    /// order once there is room.
    inbox: VecDeque<Operation>,
    /// Reusable plaintext bounce buffers. Replenished from completed ops; allocates on miss.
    pub pool: BufferPool,
    next_seq: u64,
}

impl<H: HardwareImpl> Pipeline<H> {
    pub fn new(hw: H) -> Self {
        Self {
            hw,
            ops: BTreeMap::new(),
            sdmmc_running: None,
            crypt_running: None,
            inbox: VecDeque::new(),
            pool: BufferPool::new(),
            next_seq: 0,
        }
    }

    fn ranges_overlap(a: BlockRange, b: BlockRange) -> bool {
        let a_end = (a.start as u64) + (a.count as u64);
        let b_end = (b.start as u64) + (b.count as u64);
        (a.start as u64) < b_end && (b.start as u64) < a_end
    }

    fn alloc_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }
}

impl<H: HardwareImpl> Pipeline<H> {
    pub fn is_idle(&self) -> bool {
        self.ops.is_empty() && self.sdmmc_running.is_none() && self.crypt_running.is_none()
    }

    pub fn inbox_is_empty(&self) -> bool { self.inbox.is_empty() }

    /// True when new ops must be deferred to the inbox rather than admitted directly. Triggered
    /// by reaching the concurrency threshold. (Flush ordering is enforced by the seq+range gate
    /// in `schedule_sdmmc`, not here.)
    fn pipeline_full(&self) -> bool {
        let running = self.sdmmc_running.is_some() as usize + self.crypt_running.is_some() as usize;
        self.ops.len() + running >= PIPELINE_DEFER_THRESHOLD
    }

    /// Route an op through the back-pressure gate and advance the pipeline. `range` is the flash
    /// extent the op touches; Flush callers pass `FULL_RANGE`.
    pub fn admit(&mut self, op: OperationType, range: BlockRange) {
        // Sample the idle state *before* the push so a previously-idle pipeline observes the
        // empty→busy transition. (try_advance can't see this transition because by the time it
        // runs, the new op is already in the queue.)
        if self.is_idle() {
            self.hw.cancel_idle();
        }
        let s = Operation { seq: self.alloc_seq(), range, op };
        if !self.inbox.is_empty() || self.pipeline_full() {
            self.inbox.push_back(s);
        } else {
            self.ops.insert(s.seq, s);
        }
        self.try_advance();
    }

    /// Move ops from the inbox into the active queue while there is room.
    fn drain_inbox(&mut self) {
        while !self.inbox.is_empty() && !self.pipeline_full() {
            let s = self.inbox.pop_front().unwrap();
            self.ops.insert(s.seq, s);
        }
    }

    /// Is there any op (in slots or queue) earlier than `x` whose range overlaps `x.range`?
    /// Such an op gates `x`'s SDMMC start: its prior hardware/response work must complete first.
    fn has_earlier_overlap(&self, x: &Operation) -> bool {
        let mut earlier = self
            .ops
            .range(..x.seq)
            .map(|(_, s)| s)
            .chain(self.sdmmc_running.iter())
            .chain(self.crypt_running.iter());
        earlier.any(|y| y.seq < x.seq && Self::ranges_overlap(y.range, x.range))
    }

    /// Schedule one SDMMC-step op if the slot is free. Picks the earliest-admitted eligible op
    /// not blocked by an earlier overlapping op (per the seq+range gate).
    fn schedule_sdmmc(&mut self) {
        if self.sdmmc_running.is_some() {
            return;
        }
        let pick = self
            .ops
            .iter()
            .find(|(_, s)| s.op.is_sdmmc_step() && !self.has_earlier_overlap(s))
            .map(|(seq, _)| *seq);
        if let Some(seq) = pick {
            let s = self.ops.remove(&seq).unwrap();
            self.run_sdmmc(s);
        }
    }

    /// Schedule one crypt-step op if the slot is free. Crypt operates on per-op buffers — there
    /// is no inter-op data hazard on the crypt channel, so it isn't gated by the SDMMC overlap
    /// rule. (The eventual SDMMC step of any later op is still serialized by the gate.)
    fn schedule_crypt(&mut self) {
        if self.crypt_running.is_some() {
            return;
        }
        let pick = self.ops.iter().find(|(_, s)| s.op.is_crypt_step()).map(|(seq, _)| *seq);
        if let Some(seq) = pick {
            let s = self.ops.remove(&seq).unwrap();
            self.run_crypt(s);
        }
    }

    /// Drain any Flush barriers at the head of the queue once all earlier work has completed.
    /// Returns whether the card was asked for its status.
    fn drain_flushes(&mut self) -> bool {
        if self.sdmmc_running.is_some() {
            return false;
        }
        let mut checked = false;
        while let Some((_, head)) = self.ops.iter().next() {
            if !head.op.is_flush() {
                break;
            }
            let flush_seq = head.seq;
            if let Some(crypt) = &self.crypt_running {
                if crypt.seq < flush_seq {
                    break;
                }
            }
            let s = self.ops.remove(&flush_seq).unwrap();
            if let OperationType::Flush { deferred } = s.op {
                // Ask before responding, or else the barrier reports success for writes the card
                // has not confirmed. One answer covers the whole batch.
                if !checked {
                    self.hw.assert_card_healthy();
                    checked = true;
                }
                deferred.respond(()).ok();
            }
        }
        checked
    }

    fn try_advance(&mut self) {
        let checked = self.drain_flushes();
        self.drain_inbox();
        self.schedule_sdmmc();
        self.schedule_crypt();

        if self.is_idle() {
            // Going idle is the last chance to notice a failure the card only reports on request,
            // and it covers the writes nobody flushed.
            if !checked {
                self.hw.assert_card_healthy();
            }
            self.hw.idle();
        }
    }

    fn run_sdmmc(&mut self, s: Operation) {
        let (buf_ptr, direction) = match &s.op {
            OperationType::PlainReadSdmmc { dma_buf, .. } => (dma_buf.as_mut_ptr(), Direction::Read),
            OperationType::EncReadSdmmc { ciphertext, .. } => (ciphertext.as_mut_ptr(), Direction::Read),
            OperationType::PlainWriteSdmmc { owned_buf } => (owned_buf.as_mut_ptr(), Direction::Write),
            OperationType::EncWriteSdmmc { ciphertext } => (ciphertext.as_mut_ptr(), Direction::Write),
            _ => panic!("run_sdmmc called on non-SDMMC op"),
        };

        let range = s.range;
        self.sdmmc_running = Some(s);
        self.hw.sdmmc_dma(direction, range.start, range.count, buf_ptr);
    }

    /// Send exactly one AES sub-op (one XTS cluster at most). op must be a *Crypt variant.
    fn run_crypt(&mut self, s: Operation) {
        use crypto::AES_BLOCK_SIZE;

        let range = s.range;
        let (src, dst, dir, offset) = match &s.op {
            OperationType::EncReadCrypt { ciphertext, deferred, crypt_offset, .. } => {
                (**ciphertext, deferred.body().buf, crypto::Direction::Decrypt, *crypt_offset)
            }
            OperationType::EncWriteCrypt { ciphertext, deferred, crypt_offset, .. } => {
                (deferred.body().buf, **ciphertext, crypto::Direction::Encrypt, *crypt_offset)
            }
            _ => panic!("run_crypt called on non-crypt op"),
        };

        let CryptSubOp { block_idx, block_within, len, .. } = crypt_sub_op(range.start, range.count, offset);
        let j = block_within * BLOCK_SIZE / AES_BLOCK_SIZE;

        let mut tweak = [0u8; 16];
        tweak[..4].copy_from_slice(&((block_idx / XTS_CLUSTER_BLOCKS) as u32).to_le_bytes());

        let src_sub = src.subrange(offset, len).unwrap();
        let dst_sub = dst.subrange(offset, len).unwrap();
        self.hw.disk_encrypt(tweak, j, src_sub, dst_sub, dir);

        self.crypt_running = Some(s);
    }

    pub fn on_sdmmc_done(&mut self, error_status: u32) {
        assert_eq!(error_status, 0, "SDMMC transfer error, EISTR {error_status:#06x}");

        let Operation { seq, range, op } = match self.sdmmc_running.take() {
            Some(s) => s,
            None => {
                log::warn!("SdmmcDone with no running op");
                return;
            }
        };

        match op {
            OperationType::PlainReadSdmmc { dma_buf, mut deferred } => {
                if let DmaBuf::Owned(buf) = dma_buf {
                    let mut dest = deferred.body().buf;
                    let len_words = range.count * BLOCK_SIZE / core::mem::size_of::<u32>();
                    dest.as_slice_mut::<u32>()[..len_words]
                        .copy_from_slice(&buf.as_slice::<u32>()[..len_words]);
                    xous::flush_cache(*buf, xous::CacheOperation::Invalidate).ok();
                    self.pool.release(buf);
                }
                deferred.set_response(Ok(range.count));
            }
            OperationType::EncReadSdmmc { ciphertext, deferred } => {
                let follow = OperationType::EncReadCrypt { ciphertext, crypt_offset: 0, deferred };
                self.ops.insert(seq, Operation { seq, range, op: follow });
            }
            OperationType::PlainWriteSdmmc { owned_buf } => {
                self.pool.release(owned_buf);
            }
            OperationType::EncWriteSdmmc { ciphertext } => {
                self.pool.release(ciphertext);
            }
            other => {
                log::error!("SdmmcDone for non-SDMMC op");
                self.sdmmc_running = Some(Operation { seq, range, op: other });
                return;
            }
        }

        self.try_advance();
    }

    pub fn on_disk_encrypt_complete(&mut self) {
        let Operation { seq, range, mut op } = match self.crypt_running.take() {
            Some(s) => s,
            None => {
                log::warn!("DiskEncryptComplete with no running crypt op");
                return;
            }
        };

        // Advance crypt_offset by the sub-op just completed and check if done.
        let is_done = match &mut op {
            OperationType::EncReadCrypt { crypt_offset, .. }
            | OperationType::EncWriteCrypt { crypt_offset, .. } => {
                *crypt_offset += crypt_sub_op(range.start, range.count, *crypt_offset).len;
                *crypt_offset >= range.count * BLOCK_SIZE
            }
            _ => panic!("crypt_running contained a non-crypt op: invariant violated"),
        };

        if !is_done {
            self.run_crypt(Operation { seq, range, op });
            return;
        }

        match op {
            OperationType::EncReadCrypt { ciphertext, mut deferred, .. } => {
                deferred.set_response(Ok(range.count));
                self.pool.release(ciphertext);
            }
            OperationType::EncWriteCrypt { ciphertext, mut deferred, .. } => {
                deferred.set_response(Ok(range.count));
                let follow = OperationType::EncWriteSdmmc { ciphertext };
                self.ops.insert(seq, Operation { seq, range, op: follow });
            }
            _ => unreachable!(),
        }

        self.try_advance();
    }

    /// Suspend handler: power down SDMMC if idle.
    #[allow(dead_code)]
    pub fn handle_suspend(&mut self) {
        if !self.is_idle() {
            log::debug!("Suspend arrived while not idle - ignoring");
            return;
        }
        self.hw.suspend_sdmmc();
    }
}

/// Geometry of the next crypt sub-op (at most one XTS cluster).
struct CryptSubOp {
    /// Absolute block index of the first block in the sub-op.
    block_idx: usize,
    /// Position of `block_idx` within its XTS cluster.
    block_within: usize,
    /// Byte length of this sub-op (`blocks * BLOCK_SIZE`).
    len: usize,
}

/// Compute the geometry of the next crypt sub-op for an op covering `[range_start ..
/// range_start + range_count)`, having already consumed `offset` bytes.
fn crypt_sub_op(range_start: u32, range_count: usize, offset: usize) -> CryptSubOp {
    let block_idx = range_start as usize + offset / BLOCK_SIZE;
    let block_within = block_idx % XTS_CLUSTER_BLOCKS;
    let blocks = (XTS_CLUSTER_BLOCKS - block_within).min(range_count - offset / BLOCK_SIZE);
    CryptSubOp { block_idx, block_within, len: blocks * BLOCK_SIZE }
}

/// Pool of fixed-size plaintext bounce buffers. Every buffer is `POOL_BUFFER_BYTES`, so the
/// pool is a single bucket and any well-formed request can be satisfied by any free buffer.
/// `acquire` pops a free buffer or allocates a new one on miss; `release` returns the buffer
/// for reuse rather than unmapping it.
///
/// Cache invariant: pool buffers are always invalidated (no live cache lines) while in the
/// pool. Any code path that touches a pool buffer with the CPU (e.g. memcpy) must restore the
/// invariant before `release` — `Invalidate` after a CPU read, `CleanAndInvalidate` after a
/// CPU write. This lets DMA readers/writers and consumers downstream skip per-op flushes.
pub struct BufferPool {
    free: Vec<DropDeallocate>,
}

impl BufferPool {
    pub fn new() -> Self { Self { free: Vec::new() } }

    pub fn acquire(&mut self) -> Result<DropDeallocate, EmmcError> {
        if let Some(buf) = self.free.pop() {
            return Ok(buf);
        }
        let range = xous::map_memory(
            None,
            None,
            POOL_BUFFER_BYTES,
            MemoryFlags::W | MemoryFlags::POPULATE | MemoryFlags::PLAINTEXT,
        )
        .map_err(EmmcError::from)?;
        xous::flush_cache(range, xous::CacheOperation::Invalidate).ok();
        Ok(DropDeallocate::new(range))
    }

    pub fn release(&mut self, buf: DropDeallocate) {
        debug_assert_eq!(buf.len(), POOL_BUFFER_BYTES);
        self.free.push(buf);
    }
}

/// In-memory mocks of the framework deferred types and `HardwareImpl`. Active under `cfg(test)`
/// so unit tests can drive the pipeline state machine without a real Xous server or hardware.
#[cfg(test)]
pub mod test_support {
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use server::LendMut;

    use super::{Direction, HardwareImpl};

    /// Shared flag set when a deferred carrier has been responded to. Tests assert *whether* a
    /// response was returned, not the value — pipeline behavior under test doesn't depend on the
    /// response payload.
    pub type Returned = Rc<AtomicBool>;

    pub fn returned() -> Returned { Rc::new(AtomicBool::new(false)) }

    pub fn mark(flag: &Returned) { flag.store(true, Ordering::SeqCst); }

    pub fn was_returned(flag: &Returned) -> bool { flag.load(Ordering::SeqCst) }

    /// Mirrors the slice of [`server::DeferredLendMut`] the pipeline actually uses: read access to
    /// the body and a single `set_response` setter. The shared `returned` flag flips on response.
    pub struct MockLendMut<M: LendMut> {
        body: M,
        returned: Returned,
    }

    impl<M: LendMut> MockLendMut<M> {
        pub fn new(body: M) -> (Self, Returned) {
            let flag = returned();
            (Self { body, returned: flag.clone() }, flag)
        }

        pub fn body(&self) -> &M { &self.body }

        #[allow(dead_code)]
        pub fn body_mut(&mut self) -> &mut M { &mut self.body }

        pub fn set_response(&mut self, _response: M::Response) { mark(&self.returned); }
    }

    /// Mirrors `BlockingScalarResponse::respond` (consume on respond).
    pub struct MockBlockingResponse<R> {
        returned: Returned,
        _ph: std::marker::PhantomData<R>,
    }

    impl<R> MockBlockingResponse<R> {
        pub fn new() -> (Self, Returned) {
            let flag = returned();
            (Self { returned: flag.clone(), _ph: std::marker::PhantomData }, flag)
        }

        pub fn respond(self, _value: R) -> Result<(), ()> {
            mark(&self.returned);
            Ok(())
        }
    }

    /// Records every `HardwareImpl` call in arrival order. Tests assert on `calls`.
    #[derive(Debug, Default)]
    pub struct MockHardware {
        pub calls: Vec<HardwareCall>,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum HardwareCall {
        SdmmcDma { direction: Direction, block_index: u32, blocks: usize },
        DiskEncrypt { tweak: [u8; 16], j: usize, dir: crypto::Direction, len: usize },
        AssertCardHealthy,
        ArmIdleTimer,
        DisarmIdleTimer,
        SuspendSdmmc,
    }

    impl HardwareImpl for MockHardware {
        fn sdmmc_dma(&mut self, direction: Direction, block_index: u32, blocks: usize, _buf_ptr: *mut u8) {
            self.calls.push(HardwareCall::SdmmcDma { direction, block_index, blocks });
        }

        fn assert_card_healthy(&mut self) { self.calls.push(HardwareCall::AssertCardHealthy); }

        fn disk_encrypt(
            &mut self,
            tweak: [u8; 16],
            j: usize,
            src: xous::MemoryRange,
            _dst: xous::MemoryRange,
            dir: crypto::Direction,
        ) {
            self.calls.push(HardwareCall::DiskEncrypt { tweak, j, dir, len: src.len() });
        }

        fn idle(&mut self) { self.calls.push(HardwareCall::ArmIdleTimer); }

        fn cancel_idle(&mut self) { self.calls.push(HardwareCall::DisarmIdleTimer); }

        fn suspend_sdmmc(&mut self) { self.calls.push(HardwareCall::SuspendSdmmc); }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{was_returned, HardwareCall, MockHardware, MockLendMut, Returned};
    use super::*;

    /// SDMMC_EISTR bit for a data CRC error. Which bit it is makes no difference to the pipeline,
    /// it only ever asks the hardware whether the transfer was clean.
    const EISTR_DATCRC: u32 = 0x1 << 5;

    fn pipeline() -> Pipeline<MockHardware> { Pipeline::new(MockHardware::default()) }

    fn alloc_caller_buf(blocks: usize) -> MemoryRange {
        xous::map_memory(None, None, blocks * BLOCK_SIZE, MemoryFlags::W | MemoryFlags::POPULATE).unwrap()
    }

    fn admit_plain_read(pipe: &mut Pipeline<MockHardware>, start: u32, count: usize) -> Returned {
        let buf = alloc_caller_buf(count);
        let owned = pipe.pool.acquire().unwrap();
        let (deferred, returned) =
            MockLendMut::new(ReadBlocks { buf, block_index: start, block_count: count });
        pipe.admit(
            OperationType::PlainReadSdmmc { dma_buf: DmaBuf::Owned(owned), deferred },
            BlockRange { start, count },
        );
        returned
    }

    fn admit_plain_write(pipe: &mut Pipeline<MockHardware>, start: u32, count: usize) {
        // Plain writes set their deferred response in the handler before admit, so the pipeline
        // never sees the message — only the owned buffer.
        let owned = pipe.pool.acquire().unwrap();
        pipe.admit(OperationType::PlainWriteSdmmc { owned_buf: owned }, BlockRange { start, count });
    }

    fn admit_enc_read(pipe: &mut Pipeline<MockHardware>, start: u32, count: usize) -> Returned {
        let dest_buf = alloc_caller_buf(count);
        let ciphertext = pipe.pool.acquire().unwrap();
        let (deferred, returned) =
            MockLendMut::new(ReadEncryptedBlocks { buf: dest_buf, block_index: start, block_count: count });
        pipe.admit(OperationType::EncReadSdmmc { ciphertext, deferred }, BlockRange { start, count });
        returned
    }

    fn admit_enc_write(pipe: &mut Pipeline<MockHardware>, start: u32, count: usize) -> Returned {
        let buf = alloc_caller_buf(count);
        let ciphertext = pipe.pool.acquire().unwrap();
        let (deferred, returned) =
            MockLendMut::new(WriteEncryptedBlocks { buf, block_index: start, block_count: count });
        pipe.admit(
            OperationType::EncWriteCrypt { ciphertext, crypt_offset: 0, deferred },
            BlockRange { start, count },
        );
        returned
    }

    fn admit_flush(pipe: &mut Pipeline<MockHardware>) -> Returned {
        let (resp, returned) = test_support::MockBlockingResponse::new();
        pipe.admit(OperationType::Flush { deferred: resp }, FULL_RANGE);
        returned
    }

    fn sdmmc_runs(hw: &MockHardware) -> Vec<(Direction, u32, usize)> {
        hw.calls
            .iter()
            .filter_map(|c| match c {
                HardwareCall::SdmmcDma { direction, block_index, blocks } => {
                    Some((*direction, *block_index, *blocks))
                }
                _ => None,
            })
            .collect()
    }

    fn card_checks(hw: &MockHardware) -> usize {
        hw.calls.iter().filter(|c| matches!(c, HardwareCall::AssertCardHealthy)).count()
    }

    fn crypt_runs(hw: &MockHardware) -> Vec<(crypto::Direction, usize)> {
        hw.calls
            .iter()
            .filter_map(|c| match c {
                HardwareCall::DiskEncrypt { dir, len, .. } => Some((*dir, *len)),
                _ => None,
            })
            .collect()
    }

    #[derive(Debug, PartialEq, Eq)]
    enum PowerEvent {
        Arm,
        Disarm,
        Suspend,
    }

    fn power_events(hw: &MockHardware) -> Vec<PowerEvent> {
        hw.calls
            .iter()
            .filter_map(|c| match c {
                HardwareCall::ArmIdleTimer => Some(PowerEvent::Arm),
                HardwareCall::DisarmIdleTimer => Some(PowerEvent::Disarm),
                HardwareCall::SuspendSdmmc => Some(PowerEvent::Suspend),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn empty_pipeline_is_idle() {
        let pipe = pipeline();
        assert!(pipe.is_idle());
        assert_eq!(pipe.ops.len(), 0);
        assert!(pipe.sdmmc_running.is_none());
        assert!(pipe.crypt_running.is_none());
    }

    #[test]
    fn ranges_overlap_basic() {
        let a = BlockRange { start: 10, count: 5 };
        let b = BlockRange { start: 12, count: 1 };
        let c = BlockRange { start: 20, count: 1 };
        assert!(Pipeline::<MockHardware>::ranges_overlap(a, b));
        assert!(!Pipeline::<MockHardware>::ranges_overlap(a, c));
    }

    #[test]
    fn flush_responds_immediately_when_idle() {
        let mut pipe = pipeline();
        let flush = admit_flush(&mut pipe);
        assert!(was_returned(&flush));
        assert!(pipe.ops.is_empty());
        assert!(!pipe.hw.calls.iter().any(|c| matches!(c, HardwareCall::SdmmcDma { .. })));
        // Admit always disarms a previously-idle pipeline; the synchronous flush response then
        // re-arms.
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    // --- write+read disjoint: order doesn't matter, but both must run + the read deferred must
    // be set ---

    #[test]
    fn plain_write_then_read_disjoint_both_run() {
        let mut pipe = pipeline();
        admit_plain_write(&mut pipe, 0, 4);
        let read = admit_plain_read(&mut pipe, 100, 4);

        pipe.on_sdmmc_done(0);
        pipe.on_sdmmc_done(0);

        let runs = sdmmc_runs(&pipe.hw);
        assert_eq!(runs.len(), 2, "expected two SDMMC calls, got {:?}", runs);
        assert!(runs.contains(&(Direction::Write, 0, 4)));
        assert!(runs.contains(&(Direction::Read, 100, 4)));
        assert!(was_returned(&read));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    #[test]
    fn enc_write_then_read_disjoint_both_complete() {
        fn test_disjoint(completion: impl FnOnce(&mut Pipeline<MockHardware>)) {
            let mut pipe = pipeline();
            let write = admit_enc_write(&mut pipe, 0, 4);
            let read = admit_enc_read(&mut pipe, 100, 4);

            completion(&mut pipe);
            let sd = sdmmc_runs(&pipe.hw);
            assert!(sd.contains(&(Direction::Write, 0, 4)));
            assert!(sd.contains(&(Direction::Read, 100, 4)));
            assert_eq!(crypt_runs(&pipe.hw).len(), 2);
            assert!(was_returned(&write));
            assert!(was_returned(&read));
            assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
        }

        test_disjoint(|pipe| {
            pipe.on_sdmmc_done(0);
            pipe.on_disk_encrypt_complete();
            pipe.on_sdmmc_done(0);
            pipe.on_disk_encrypt_complete();
        });
        test_disjoint(|pipe| {
            pipe.on_disk_encrypt_complete();
            pipe.on_sdmmc_done(0);
            pipe.on_disk_encrypt_complete();
            pipe.on_sdmmc_done(0);
        });
        test_disjoint(|pipe| {
            pipe.on_disk_encrypt_complete();
            pipe.on_sdmmc_done(0);
            pipe.on_sdmmc_done(0);
            pipe.on_disk_encrypt_complete();
        });
        test_disjoint(|pipe| {
            pipe.on_sdmmc_done(0);
            pipe.on_disk_encrypt_complete();
            pipe.on_disk_encrypt_complete();
            pipe.on_sdmmc_done(0);
        });
    }

    // --- write+read overlapping: write SDMMC must precede read SDMMC ---

    #[test]
    fn plain_write_then_read_overlapping_write_sdmmc_first() {
        let mut pipe = pipeline();
        admit_plain_write(&mut pipe, 10, 5);
        let read = admit_plain_read(&mut pipe, 12, 1);

        // Before any completion, exactly one SDMMC call: the write.
        let runs = sdmmc_runs(&pipe.hw);
        assert_eq!(runs, vec![(Direction::Write, 10, 5)]);
        assert!(!was_returned(&read));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm]);

        pipe.on_sdmmc_done(0);
        let runs = sdmmc_runs(&pipe.hw);
        assert_eq!(runs, vec![(Direction::Write, 10, 5), (Direction::Read, 12, 1)]);
        assert!(!was_returned(&read), "read deferred set before its SDMMC completed");
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm]);

        pipe.on_sdmmc_done(0);
        assert!(was_returned(&read));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    #[test]
    fn enc_write_then_read_overlapping_crypt_done_first() {
        let mut pipe = pipeline();
        let write = admit_enc_write(&mut pipe, 10, 5);
        let read = admit_enc_read(&mut pipe, 12, 1);

        // Simulate completion of read and write.
        // There is only one proper order of operations.
        assert_eq!(sdmmc_runs(&pipe.hw).len(), 0);
        assert_eq!(crypt_runs(&pipe.hw).len(), 1);

        pipe.on_disk_encrypt_complete();
        assert_eq!(sdmmc_runs(&pipe.hw).len(), 1);
        assert_eq!(crypt_runs(&pipe.hw).len(), 1);

        pipe.on_sdmmc_done(0);
        assert_eq!(sdmmc_runs(&pipe.hw).len(), 2);
        assert_eq!(crypt_runs(&pipe.hw).len(), 1);

        pipe.on_sdmmc_done(0);
        assert_eq!(sdmmc_runs(&pipe.hw).len(), 2);
        assert_eq!(crypt_runs(&pipe.hw).len(), 2);

        pipe.on_disk_encrypt_complete();

        let sd = sdmmc_runs(&pipe.hw);
        assert_eq!(sd, vec![(Direction::Write, 10, 5), (Direction::Read, 12, 1)]);
        assert!(was_returned(&write));
        assert!(was_returned(&read));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    // --- write+write overlapping: SDMMC must run in admit order ---

    #[test]
    fn plain_write_then_write_overlapping_in_admit_order() {
        let mut pipe = pipeline();
        admit_plain_write(&mut pipe, 10, 5);
        admit_plain_write(&mut pipe, 12, 1);

        let runs = sdmmc_runs(&pipe.hw);
        assert_eq!(runs, vec![(Direction::Write, 10, 5)]);
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm]);

        pipe.on_sdmmc_done(0);
        pipe.on_sdmmc_done(0);
        assert_eq!(sdmmc_runs(&pipe.hw), vec![(Direction::Write, 10, 5), (Direction::Write, 12, 1)]);
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    #[test]
    fn enc_write_then_write_overlapping_in_admit_order() {
        let mut pipe = pipeline();
        let write_a = admit_enc_write(&mut pipe, 10, 5);
        let write_b = admit_enc_write(&mut pipe, 12, 1);

        pipe.on_disk_encrypt_complete();
        pipe.on_sdmmc_done(0);
        pipe.on_disk_encrypt_complete();
        pipe.on_sdmmc_done(0);

        let sd = sdmmc_runs(&pipe.hw);
        assert_eq!(sd, vec![(Direction::Write, 10, 5), (Direction::Write, 12, 1)]);
        assert!(was_returned(&write_a));
        assert!(was_returned(&write_b));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    // --- read+read: order doesn't matter ---

    #[test]
    fn plain_read_read_both_complete() {
        let mut pipe = pipeline();
        let read_a = admit_plain_read(&mut pipe, 0, 4);
        let read_b = admit_plain_read(&mut pipe, 100, 4);

        pipe.on_sdmmc_done(0);
        pipe.on_sdmmc_done(0);

        let runs = sdmmc_runs(&pipe.hw);
        assert_eq!(runs.len(), 2);
        assert!(runs.contains(&(Direction::Read, 0, 4)));
        assert!(runs.contains(&(Direction::Read, 100, 4)));
        assert!(was_returned(&read_a));
        assert!(was_returned(&read_b));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    #[test]
    fn enc_read_read_both_complete() {
        let mut pipe = pipeline();
        let read_a = admit_enc_read(&mut pipe, 0, 4);
        let read_b = admit_enc_read(&mut pipe, 100, 4);

        pipe.on_sdmmc_done(0);
        pipe.on_disk_encrypt_complete();
        pipe.on_sdmmc_done(0);
        pipe.on_disk_encrypt_complete();

        let sd = sdmmc_runs(&pipe.hw);
        assert!(sd.contains(&(Direction::Read, 0, 4)));
        assert!(sd.contains(&(Direction::Read, 100, 4)));
        assert_eq!(crypt_runs(&pipe.hw).len(), 2);
        assert!(was_returned(&read_a));
        assert!(was_returned(&read_b));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    // --- a read that crosses an XTS cluster boundary must produce two crypt sub-ops, each
    // capped at one cluster, before the deferred returns. ---

    #[test]
    fn enc_read_crossing_xts_boundary_runs_two_crypts() {
        const SUB_BLOCKS: usize = 4;
        let start = (XTS_CLUSTER_BLOCKS - SUB_BLOCKS) as u32;
        let count = SUB_BLOCKS * 2;
        let mut pipe = pipeline();
        let read = admit_enc_read(&mut pipe, start, count);

        // Single SDMMC read fetches both halves; no crypt yet.
        assert_eq!(sdmmc_runs(&pipe.hw), vec![(Direction::Read, start, count)]);
        assert_eq!(crypt_runs(&pipe.hw).len(), 0);

        // SDMMC done → first crypt sub-op fires (4 blocks, the head of the range up to the
        // cluster boundary).
        pipe.on_sdmmc_done(0);
        assert_eq!(crypt_runs(&pipe.hw), vec![(crypto::Direction::Decrypt, SUB_BLOCKS * BLOCK_SIZE)]);
        assert!(!was_returned(&read));

        // First sub-op done → pipeline immediately fires the second sub-op (the remaining 4
        // blocks past the cluster boundary). Read still not returned.
        pipe.on_disk_encrypt_complete();
        assert_eq!(
            crypt_runs(&pipe.hw),
            vec![
                (crypto::Direction::Decrypt, SUB_BLOCKS * BLOCK_SIZE),
                (crypto::Direction::Decrypt, SUB_BLOCKS * BLOCK_SIZE),
            ]
        );
        assert!(!was_returned(&read));

        // Second sub-op done → caller deferred returns.
        pipe.on_disk_encrypt_complete();
        assert!(was_returned(&read));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    // --- write + flush + (read|write): the third op may run its crypt step concurrently with
    // the flush wait (crypt has no inter-op data hazard), but its SDMMC step must wait for the
    // flush to respond. ---

    #[test]
    fn plain_write_flush_read_third_idle_until_flush() {
        let mut pipe = pipeline();
        admit_plain_write(&mut pipe, 0, 4);
        let flush_returned = admit_flush(&mut pipe);
        let read_returned = admit_plain_read(&mut pipe, 100, 4);

        // Only the write SDMMC has fired; flush + read are blocked.
        assert_eq!(sdmmc_runs(&pipe.hw), vec![(Direction::Write, 0, 4)]);
        assert!(!was_returned(&flush_returned));
        assert!(!was_returned(&read_returned));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm]);

        pipe.on_sdmmc_done(0);
        // Flush has now returned; the read may begin.
        assert!(was_returned(&flush_returned));
        let runs = sdmmc_runs(&pipe.hw);
        assert_eq!(runs, vec![(Direction::Write, 0, 4), (Direction::Read, 100, 4)]);
        assert!(!was_returned(&read_returned));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm]);

        pipe.on_sdmmc_done(0);
        assert!(was_returned(&read_returned));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    #[test]
    fn plain_write_flush_write_third_idle_until_flush() {
        let mut pipe = pipeline();
        admit_plain_write(&mut pipe, 0, 4);
        let flush = admit_flush(&mut pipe);
        admit_plain_write(&mut pipe, 100, 4);

        assert_eq!(sdmmc_runs(&pipe.hw), vec![(Direction::Write, 0, 4)]);
        assert!(!was_returned(&flush));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm]);

        pipe.on_sdmmc_done(0);
        assert!(was_returned(&flush));
        assert_eq!(sdmmc_runs(&pipe.hw), vec![(Direction::Write, 0, 4), (Direction::Write, 100, 4)]);
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm]);

        pipe.on_sdmmc_done(0);
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    #[test]
    fn enc_write_flush_read_third_sdmmc_idle_until_flush() {
        let mut pipe = pipeline();
        let _write = admit_enc_write(&mut pipe, 0, 4);
        let flush = admit_flush(&mut pipe);
        let read = admit_enc_read(&mut pipe, 100, 4);

        // Read is encrypted, so its first hardware step is SDMMC. That step must not fire before
        // the flush returns. (Crypt of the third op isn't applicable here — read crypt happens
        // after read SDMMC.)
        assert_eq!(sdmmc_runs(&pipe.hw), vec![]);
        assert!(!was_returned(&flush));
        assert!(!was_returned(&read));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm]);

        pipe.on_disk_encrypt_complete();
        pipe.on_sdmmc_done(0);

        assert!(was_returned(&flush));
        assert!(!was_returned(&read));

        pipe.on_sdmmc_done(0);
        pipe.on_disk_encrypt_complete();
        assert!(was_returned(&read));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    #[test]
    fn enc_write_flush_write_third_sdmmc_idle_until_flush() {
        let mut pipe = pipeline();
        let write_a = admit_enc_write(&mut pipe, 0, 4);
        let flush = admit_flush(&mut pipe);
        let write_b = admit_enc_write(&mut pipe, 100, 4);

        // B's SDMMC step must not fire while the flush is outstanding. B's crypt may run
        // concurrently — that's fine because crypt has no inter-op flash hazard.
        assert_eq!(sdmmc_runs(&pipe.hw), vec![]);
        assert!(!was_returned(&write_a));
        assert!(!was_returned(&flush));
        assert!(!was_returned(&write_b));
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm]);

        pipe.on_disk_encrypt_complete();
        pipe.on_sdmmc_done(0);

        assert!(was_returned(&write_a));
        assert!(was_returned(&flush));
        assert!(!was_returned(&write_b));

        pipe.on_disk_encrypt_complete();
        pipe.on_sdmmc_done(0);
        assert!(was_returned(&write_b));

        assert_eq!(sdmmc_runs(&pipe.hw), vec![(Direction::Write, 0, 4), (Direction::Write, 100, 4)]);
        assert_eq!(power_events(&pipe.hw), vec![PowerEvent::Disarm, PowerEvent::Arm]);
    }

    // --- transfer errors take the system down instead of completing the op ---

    #[test]
    #[should_panic(expected = "SDMMC transfer error")]
    fn transfer_error_panics_instead_of_completing_the_op() {
        let mut pipe = pipeline();
        admit_plain_read(&mut pipe, 0, 4);
        pipe.on_sdmmc_done(EISTR_DATCRC);
    }

    #[test]
    fn going_idle_asks_the_card() {
        let mut pipe = pipeline();
        admit_plain_write(&mut pipe, 0, 4);
        assert_eq!(card_checks(&pipe.hw), 0);

        pipe.on_sdmmc_done(0);
        assert_eq!(card_checks(&pipe.hw), 1);
    }

    #[test]
    fn consecutive_flushes_ask_the_card_once() {
        let mut pipe = pipeline();
        admit_plain_write(&mut pipe, 0, 4);
        let first = admit_flush(&mut pipe);
        let second = admit_flush(&mut pipe);

        pipe.on_sdmmc_done(0);
        assert!(was_returned(&first));
        assert!(was_returned(&second));
        assert_eq!(card_checks(&pipe.hw), 1, "the batch and the idle transition share one answer");
    }
}
