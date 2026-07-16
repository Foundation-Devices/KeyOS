use alloc::{vec, vec::Vec};
use core::fmt::Debug;

use zerocopy::{FromBytes, Immutable, IntoBytes};

use crate::{
    pool::{Pool, PoolElementHandle},
    queue::EndpointDirection,
    registers::{PidCode, Qtd, QueueHead},
    util::VolatileCellHelper,
    EhciError, TransferContext,
};

extern crate alloc;

/// A single (to be sent) USB transfer.
pub struct Transfer<CT> {
    endpoint: u8,
    direction: EndpointDirection,
    context: CT,
    qtds: Vec<PoolElementHandle<Qtd>>,
    last_data_qtd_index: usize,
    original_total_bytes: usize,
}

/// Results of a single USB transfer
#[derive(Debug)]
pub struct TransferResult<CT> {
    /// The received (or sent) data bytes. May be less than
    /// what was requested.
    /// (e.g. a short packet was received, or the endpoint
    /// stalled before completing an OUT transaction)
    pub result: Result<usize, EhciError>,
    /// User-provided context, used to pair Transfers to Results.
    pub context: CT,
}

/// Data for a standard USB Control Request
#[repr(C, packed)]
#[derive(Clone, Debug, Default, Immutable, IntoBytes, FromBytes)]
pub struct ControlRequest {
    /// See bmRequestType in the USB 2.0 standard
    pub typ: u8,
    /// See bRequest in the USB 2.0 standard
    pub request: u8,
    /// See wValue in the USB 2.0 standard
    pub value: u16,
    /// See wIndex in the USB 2.0 standard
    pub index: u16,
    /// Number of bytes to transfer during reading or writing.
    /// Should be the same as data.len() length when writing.
    pub length: u16,
}

impl ControlRequest {
    pub(crate) fn into_tmp_buffer(self) -> [u8; 0x40] {
        let mut result = [0u8; 0x40];
        result[..self.as_bytes().len()].copy_from_slice(self.as_bytes());
        result
    }
}

macro_rules! alloc {
    ($qtd_pool:ident, $type:ident, $data:expr, $virt_to_phys:ident, $context:ident) => {
        match $qtd_pool.alloc(Qtd::new(PidCode::$type, $data, &$virt_to_phys)) {
            Ok(qtd) => qtd,
            Err(e) => return Err(($context, e)),
        }
    };
}

impl<CT: TransferContext> Transfer<CT> {
    /// Standard control write transfer.
    ///
    /// Also used for no-data, if data is empty
    pub fn new_control_write(
        qtd_pool: &mut Pool<Qtd>,
        mut context: CT,
        virt_to_phys: impl Fn(*const u8) -> usize,
    ) -> Result<Self, (CT, EhciError)> {
        let mut qtds = vec![alloc!(qtd_pool, Setup, context.setup_buffer(), virt_to_phys, context)];
        let mut data_qtd_index = 0;
        let mut original_total_bytes = 8;
        let data = context.data_buffer();
        if !data.is_empty() {
            qtds.push(alloc!(qtd_pool, Out, &data, virt_to_phys, context));
            data_qtd_index = 1;
            original_total_bytes = data.len();
        }
        qtds.push(alloc!(qtd_pool, In, &[], virt_to_phys, context));
        for i in 0..qtds.len() - 1 {
            qtds[i].next.set(qtds[i + 1].to_controller_ptr());
        }
        Ok(Self {
            context,
            qtds,
            endpoint: 0,
            direction: EndpointDirection::Out,
            last_data_qtd_index: data_qtd_index,
            original_total_bytes,
        })
    }

    /// Standard control read transfer.
    pub fn new_control_read(
        qtd_pool: &mut Pool<Qtd>,
        mut context: CT,
        virt_to_phys: impl Fn(*const u8) -> usize,
    ) -> Result<Self, (CT, EhciError)> {
        let setup_qtd = alloc!(qtd_pool, Setup, &context.setup_buffer(), virt_to_phys, context);
        let data_qtd = alloc!(qtd_pool, In, context.data_buffer(), virt_to_phys, context);
        let status_qtd = alloc!(qtd_pool, Out, &[], virt_to_phys, context);
        setup_qtd.next.set(data_qtd.to_controller_ptr());
        data_qtd.next.set(status_qtd.to_controller_ptr());
        Ok(Self {
            qtds: vec![setup_qtd, data_qtd, status_qtd],
            endpoint: 0,
            // Control transfers are special: we don't use the direction to
            // choose a queue, reads and writes are the same endpoint (0)
            direction: EndpointDirection::Out,
            last_data_qtd_index: 1,
            original_total_bytes: context.data_buffer().len(),
            context,
        })
    }

    /// Standard bulk write transfer.
    pub fn new_bulk_out(
        qtd_pool: &mut Pool<Qtd>,
        endpoint: u8,
        mut context: CT,
        virt_to_phys: impl Fn(*const u8) -> usize,
    ) -> Result<Self, (CT, EhciError)> {
        let mut qtds = Vec::new();
        if context.data_buffer().is_empty() {
            qtds.push(alloc!(qtd_pool, Out, &[], virt_to_phys, context));
        } else {
            for chunk in context.data_buffer().chunks(0x4000) {
                qtds.push(alloc!(qtd_pool, Out, chunk, virt_to_phys, context));
            }
        }
        for i in 0..qtds.len() - 1 {
            qtds[i].next.set(qtds[i + 1].to_controller_ptr());
        }
        Ok(Self {
            last_data_qtd_index: qtds.len() - 1,
            qtds,
            endpoint,
            direction: EndpointDirection::Out,
            original_total_bytes: context.data_buffer().len(),
            context,
        })
    }

    /// Standard bulk read transfer.
    pub fn new_bulk_in(
        qtd_pool: &mut Pool<Qtd>,
        endpoint: u8,
        mut context: CT,
        virt_to_phys: impl Fn(*const u8) -> usize,
    ) -> Result<Self, (CT, EhciError)> {
        let mut qtds = Vec::new();
        if context.data_buffer().is_empty() {
            qtds.push(alloc!(qtd_pool, In, &[], virt_to_phys, context));
        } else {
            for chunk in context.data_buffer().chunks(0x4000) {
                qtds.push(alloc!(qtd_pool, In, chunk, virt_to_phys, context));
            }
        }
        for i in 0..qtds.len() - 1 {
            qtds[i].next.set(qtds[i + 1].to_controller_ptr());
        }
        Ok(Self {
            last_data_qtd_index: qtds.len() - 1,
            qtds,
            endpoint,
            direction: EndpointDirection::In,
            original_total_bytes: context.data_buffer().len(),
            context,
        })
    }

    pub(crate) fn endpoint(&self) -> u8 { self.endpoint }

    pub(crate) fn direction(&self) -> EndpointDirection { self.direction }

    pub(crate) fn was_successful(&self) -> bool { self.all_finished() && !self.any_halted() }

    pub(crate) fn link_after(&self, other: &mut Self) {
        other.qtds.last().unwrap().next.set(self.qtds[0].to_controller_ptr());
    }

    pub(crate) fn link_into_qh(&self, qh: &mut QueueHead) {
        qh.next_qtd.set(self.qtds[0].to_controller_ptr());
    }

    pub(crate) fn any_halted(&self) -> bool { self.qtds.iter().any(|q| q.token.get().halted()) }

    pub(crate) fn all_finished(&self) -> bool { self.qtds.iter().all(|q| !q.token.get().active()) }

    pub(crate) fn halt_all(&mut self) {
        for qtd in &mut self.qtds {
            qtd.token.change(|t| {
                t.set_active(false);
                t.set_halted(true)
            });
        }
    }

    pub(crate) fn into_result(self) -> TransferResult<CT> {
        let result = if self.was_successful() {
            let remaining = self.qtds[self.last_data_qtd_index].token.get().total_bytes();
            let transferred = self.original_total_bytes.saturating_sub(remaining as usize);
            Ok(transferred)
        } else {
            Err(EhciError::Stalled)
        };
        TransferResult { result, context: self.context }
    }

    pub(crate) fn take_context(self) -> CT { self.context }
}

impl<CT: TransferContext> Debug for Transfer<CT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Transfer")
            .field("datalen", &self.original_total_bytes)
            .field("endpoint", &self.endpoint())
            .field("qtds", &self.qtds)
            .finish()
    }
}

impl Debug for PoolElementHandle<Qtd> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "0x{:x}", &**self as *const Qtd as u32)
    }
}
