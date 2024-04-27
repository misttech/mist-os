// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Transmit and Receive queue API objects.

use core::marker::PhantomData;

use crate::{
    context::ContextPair,
    device::{
        queue::{
            fifo,
            rx::{
                ReceiveDequeContext, ReceiveDequeFrameContext as _, ReceiveQueueBindingsContext,
                ReceiveQueueContext as _, ReceiveQueueState,
            },
            tx::{
                self, TransmitDequeueContext, TransmitQueueBindingsContext, TransmitQueueCommon,
                TransmitQueueConfiguration, TransmitQueueContext as _, TransmitQueueState,
            },
            DequeueResult, DequeueState, MAX_BATCH_SIZE,
        },
        socket::DeviceSocketHandler,
        Device, DeviceIdContext, DeviceSendFrameError,
    },
    work_queue::WorkQueueReport,
};

/// An API to interact with device `D` transmit queues.
pub struct TransmitQueueApi<D, C>(C, PhantomData<D>);

impl<D, C> TransmitQueueApi<D, C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx, PhantomData)
    }
}

impl<D, C> TransmitQueueApi<D, C>
where
    D: Device,
    C: ContextPair,
    C::CoreContext:
        TransmitDequeueContext<D, C::BindingsContext> + DeviceSocketHandler<D, C::BindingsContext>,
    C::BindingsContext:
        TransmitQueueBindingsContext<D, <C::CoreContext as DeviceIdContext<D>>::DeviceId>,
{
    fn contexts(&mut self) -> (&mut C::CoreContext, &mut C::BindingsContext) {
        let Self(pair, PhantomData) = self;
        pair.contexts()
    }

    /// Transmits any queued frames.
    pub fn transmit_queued_frames(
        &mut self,
        device_id: &<C::CoreContext as DeviceIdContext<D>>::DeviceId,
    ) -> Result<WorkQueueReport, DeviceSendFrameError<()>> {
        let (core_ctx, bindings_ctx) = self.contexts();
        core_ctx.with_dequed_packets_and_tx_queue_ctx(
            device_id,
            |DequeueState { dequeued_frames: dequed_packets }, tx_queue_ctx| {
                assert!(
                    dequed_packets.is_empty(),
                    "should never have left packets after attempting to dequeue"
                );

                let ret = tx_queue_ctx.with_transmit_queue_mut(
                    device_id,
                    |TransmitQueueState { allocator: _, queue }| {
                        queue.as_mut().map(|q| q.dequeue_into(dequed_packets, MAX_BATCH_SIZE))
                    },
                );

                // If we don't have a transmit queue installed, report no work
                // left to be done.
                let Some(ret) = ret else { return Ok(WorkQueueReport::AllDone) };

                while let Some((meta, p)) = dequed_packets.pop_front() {
                    tx::deliver_to_device_sockets(tx_queue_ctx, bindings_ctx, device_id, &p, &meta);

                    match tx_queue_ctx.send_frame(bindings_ctx, device_id, meta, p) {
                        Ok(()) => {}
                        Err(DeviceSendFrameError::DeviceNotReady(x)) => {
                            // We failed to send the frame so requeue it and try
                            // again later.
                            tx_queue_ctx.with_transmit_queue_mut(
                                device_id,
                                |TransmitQueueState { allocator: _, queue }| {
                                    dequed_packets.push_front(x);
                                    queue.as_mut().unwrap().requeue_items(dequed_packets);
                                },
                            );
                            return Err(DeviceSendFrameError::DeviceNotReady(()));
                        }
                    }
                }

                Ok(ret.into())
            },
        )
    }

    /// Sets the queue configuration for the device.
    pub fn set_configuration(
        &mut self,
        device_id: &<C::CoreContext as DeviceIdContext<D>>::DeviceId,
        config: TransmitQueueConfiguration,
    ) {
        let (core_ctx, bindings_ctx) = self.contexts();
        // We take the dequeue lock as well to make sure we finish any current
        // dequeuing before changing the configuration.
        core_ctx.with_dequed_packets_and_tx_queue_ctx(
            device_id,
            |DequeueState { dequeued_frames: dequed_packets }, tx_queue_ctx| {
                assert!(
                    dequed_packets.is_empty(),
                    "should never have left packets after attempting to dequeue"
                );

                let prev_queue = tx_queue_ctx.with_transmit_queue_mut(
                    device_id,
                    |TransmitQueueState { allocator: _, queue }| {
                        match config {
                            TransmitQueueConfiguration::None => core::mem::take(queue),
                            TransmitQueueConfiguration::Fifo => {
                                match queue {
                                    None => *queue = Some(fifo::Queue::default()),
                                    // Already a FiFo queue.
                                    Some(_) => {}
                                }

                                None
                            }
                        }
                    },
                );

                let Some(mut prev_queue) = prev_queue else { return };

                loop {
                    let ret = prev_queue.dequeue_into(dequed_packets, MAX_BATCH_SIZE);

                    while let Some((meta, p)) = dequed_packets.pop_front() {
                        tx::deliver_to_device_sockets(
                            tx_queue_ctx,
                            bindings_ctx,
                            device_id,
                            &p,
                            &meta,
                        );
                        match tx_queue_ctx.send_frame(bindings_ctx, device_id, meta, p) {
                            Ok(()) => {}
                            Err(DeviceSendFrameError::DeviceNotReady(x)) => {
                                // We swapped to no-queue and device cannot send
                                // the frame so we just drop it.
                                let _: (
                                    <C::CoreContext as TransmitQueueCommon<_, _>>::Meta,
                                    <C::CoreContext as TransmitQueueCommon<_, _>>::Buffer,
                                ) = x;
                            }
                        }
                    }

                    match ret {
                        DequeueResult::NoMoreLeft => break,
                        DequeueResult::MoreStillQueued => {}
                    }
                }
            },
        )
    }
}

/// /// An API to interact with device `D` receive queues.
pub struct ReceiveQueueApi<D, C>(C, PhantomData<D>);

impl<D, C> ReceiveQueueApi<D, C> {
    pub(crate) fn new(ctx: C) -> Self {
        Self(ctx, PhantomData)
    }
}

impl<D, C> ReceiveQueueApi<D, C>
where
    D: Device,
    C: ContextPair,
    C::BindingsContext:
        ReceiveQueueBindingsContext<D, <C::CoreContext as DeviceIdContext<D>>::DeviceId>,
    C::CoreContext: ReceiveDequeContext<D, C::BindingsContext>,
{
    fn contexts(&mut self) -> (&mut C::CoreContext, &mut C::BindingsContext) {
        let Self(pair, PhantomData) = self;
        pair.contexts()
    }

    /// Handle a batch of queued RX packets for the device.
    ///
    /// If packets remain in the RX queue after a batch of RX packets has been
    /// handled, the RX task will be scheduled to run again so the next batch of
    /// RX packets may be handled. See
    /// [`DeviceLayerEventDispatcher::wake_rx_task`] for more details.
    pub fn handle_queued_frames(
        &mut self,
        device_id: &<C::CoreContext as DeviceIdContext<D>>::DeviceId,
    ) -> WorkQueueReport {
        let (core_ctx, bindings_ctx) = self.contexts();
        core_ctx.with_dequed_frames_and_rx_queue_ctx(
            device_id,
            |DequeueState { dequeued_frames }, rx_queue_ctx| {
                assert_eq!(
                    dequeued_frames.len(),
                    0,
                    "should not keep dequeued frames across calls to this fn"
                );

                let ret = rx_queue_ctx.with_receive_queue_mut(
                    device_id,
                    |ReceiveQueueState { queue }| {
                        queue.dequeue_into(dequeued_frames, MAX_BATCH_SIZE)
                    },
                );

                while let Some((meta, p)) = dequeued_frames.pop_front() {
                    rx_queue_ctx.handle_frame(bindings_ctx, device_id, meta, p);
                }

                ret.into()
            },
        )
    }
}
