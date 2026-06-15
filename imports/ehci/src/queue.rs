extern crate alloc;
use alloc::{vec, vec::Vec};
use core::fmt::Debug;

use log::debug;

use crate::{
    error::EhciError,
    pool::{Pool, PoolElementHandle},
    registers::{QtdPointer, QueueHead, QueueHeadPointer},
    transfer::{Transfer, TransferResult},
    util::VolatileCellHelper, TransferContext,
};

pub struct AsyncQueue<CT> {
    /// Persistent head for the addr 0, EP 0 pipe used for setup
    heads: Vec<ActiveQueueHead<CT>>,
    head_graveyard: Vec<InactiveQueueHead>,
    transfer_graveyard: Vec<TransferResult<CT>>,
    head_pool: Pool<QueueHead>,
}

/// Direction of the endpoint
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointDirection {
    /// Host -> Device
    Out,
    /// Device -> Host
    In,
}

struct ActiveQueueHead<CT> {
    head: PoolElementHandle<QueueHead>,
    address: u8,
    endpoint: u8,
    direction: EndpointDirection,
    transfers: Vec<Transfer<CT>>,
}

struct InactiveQueueHead {
    _head: PoolElementHandle<QueueHead>,
    ticks: usize,
}

impl<CT: TransferContext> AsyncQueue<CT> {
    pub fn new(mut head_pool: Pool<QueueHead>) -> Result<Self, EhciError> {
        let addr_0_head = head_pool.alloc(Default::default())?;
        addr_0_head.next.set(addr_0_head.to_controller_ptr());
        addr_0_head.info1.change(|i| {
            i.set_is_head(true);
            i.set_max_packet_length(0x40);
            // Control channel, where we do depend on the
            // non-trivial toggle logic
            i.set_data_toggle_control(true);
        });

        Ok(Self {
            heads: vec![ActiveQueueHead {
                head: addr_0_head,
                address: 0,
                endpoint: 0,
                direction: EndpointDirection::Out,
                transfers: Vec::new(),
            }],
            head_graveyard: Vec::new(),
            transfer_graveyard: Vec::new(),
            head_pool,
        })
    }

    /// Get the QueueHead that has the H bit set
    /// # Safety: this pointer is only alive as long as this object is alive
    /// (in any form)
    pub unsafe fn head(&self) -> QueueHeadPointer { self.heads[0].head.to_controller_ptr() }

    pub fn open_endpoint(
        &mut self,
        address: u8,
        endpoint: u8,
        max_packet_length: u16,
        direction: EndpointDirection,
    ) -> Result<(), EhciError> {
        if address == 0 && (endpoint == 0) {
            return Ok(());
        }
        // TODO: Detect if EP is already open
        debug!("Opening endpoint A={address}:EP={endpoint}");
        let head = self.head_pool.alloc(Default::default())?;
        head.info1.change(|i| {
            i.set_address(address);
            i.set_endpoint(endpoint);
            i.set_max_packet_length(max_packet_length);
            if endpoint == 0 {
                i.set_data_toggle_control(true)
            }
        });
        head.next.set(self.heads[0].head.to_controller_ptr());
        self.heads.last().unwrap().head.next.set(head.to_controller_ptr());
        self.heads.push(ActiveQueueHead { head, address, endpoint, direction, transfers: Vec::new() });
        Ok(())
    }

    pub fn close_endpoint(
        &mut self,
        address: u8,
        endpoint: Option<u8>,
        direction: Option<EndpointDirection>,
    ) {
        if address == 0 && (endpoint.is_none() || endpoint == Some(0)) {
            return;
        }
        let mut i = 1;
        while i < self.heads.len() {
            if self.heads[i].address == address
                && (endpoint.is_none() || endpoint == Some(self.heads[i].endpoint))
                && (direction.is_none() || direction == Some(self.heads[i].direction))
            {
                let mut head = self.heads.remove(i);
                debug!("Closing endpoint A={}:EP={}:{:?}", head.address, head.endpoint, head.direction);

                // Halt all qtds
                for transfer in &mut head.transfers {
                    transfer.halt_all();
                }
                self.transfer_graveyard.extend(head.transfers.into_iter().map(|t| t.into_result()));

                // RIP
                self.head_graveyard.push(InactiveQueueHead { _head: head.head, ticks: 0 });

                // Actual unlinking: Set the next ptr of the previous entry
                // to the correct one. Set it to head[0] if this was the
                // last entry
                self.heads[i - 1].head.next.set(self.heads[i % self.heads.len()].head.to_controller_ptr());
            } else {
                i += 1
            }
        }
    }

    pub fn schedule_transfer(
        &mut self,
        address: u8,
        transfer: Transfer<CT>,
    ) -> Result<(), (Transfer<CT>, EhciError)> {
        let endpoint = transfer.endpoint();
        let direction = transfer.direction();
        let Some(head) = self
            .heads
            .iter_mut()
            .find(|h| h.address == address && h.endpoint == endpoint && h.direction == direction)
        else {
            debug!("Could not find queue head for A={address}:EP={endpoint}:{direction:?}");
            return Err((transfer, EhciError::EndpointNotOpen));
        };

        debug!("Scheduling transfer {transfer:?} to A={address}:EP={endpoint}:{direction:?}");

        if let Some(list_end) = head.transfers.last_mut() {
            transfer.link_after(list_end);
        } else {
            transfer.link_into_qh(&mut head.head)
        }
        head.transfers.push(transfer);
        Ok(())
    }

    /// Collect finished transfers, garbage collect, etc.
    /// Returns finished transfers.
    pub fn work(&mut self) -> Vec<TransferResult<CT>> {
        // TODO: Detect error flags in status and fully reset the controller if we have to.

        // Clear the graveyard.
        // XXX: Instead of using the Doorbell mechanism, we just wait
        //      a few updates on the queue to "make sure" the controller
        //      does not have any cached entries left.
        for head in &mut self.head_graveyard {
            head.ticks += 1;
        }
        self.head_graveyard.retain(|head| head.ticks < 3);

        let mut result = core::mem::take(&mut self.transfer_graveyard);
        for head in &mut self.heads {
            // GC all transfers that are not active anymore.
            // They shouldn't be part of the queue, as the only
            // way for them to become inactive is if the USB Host
            // sets them to inactive or halts on them.
            while !head.transfers.is_empty()
                && (head.transfers[0].all_finished() || head.transfers[0].any_halted())
            {
                let transfer = head.transfers.remove(0);
                debug!("Finished transfer {transfer:?}, success={}", transfer.was_successful());
                result.push(transfer.into_result());
            }
            let qh = &mut head.head;
            if qh.token.get().halted() {
                debug!("Qtd {:?} was halted", qh.current_qtd.get());
                // The halted Qtd should be cleared by now.
                if let Some(transfer) = head.transfers.first() {
                    transfer.link_into_qh(qh);
                } else {
                    qh.next_qtd.set(QtdPointer::TERMINATE);
                }
                qh.token.change(|t| {
                    t.set_halted(false);
                    t.set_active(false)
                });
            }
        }
        result
    }

    /// Collect finished transfers, fail unfinished ones
    pub fn flush(&mut self) -> Vec<TransferResult<CT>> {
        let mut result = core::mem::take(&mut self.transfer_graveyard);
        result.extend(
            self.heads
                .split_off(1)
                .into_iter()
                .flat_map(|head| head.transfers.into_iter().map(|t| t.into_result())),
        );
        result
    }
}

impl<CT: TransferContext> Debug for ActiveQueueHead<CT> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ActiveQueueHead")
            .field("address", &self.address)
            .field("endpoint", &self.endpoint)
            .field("info1", &self.head.info1.get())
            .field("info2", &self.head.info2.get())
            .field("current", &self.head.current_qtd.get())
            .field("next", &self.head.next_qtd.get())
            .field("token", &self.head.token.get())
            .field("transfers", &self.transfers)
            .finish()
    }
}

impl Debug for InactiveQueueHead {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InactiveQueueHead").finish()
    }
}
