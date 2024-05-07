// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! TX device queues.

use alloc::vec::Vec;
use core::convert::Infallible as Never;

use derivative::Derivative;
use packet::{
    new_buf_vec, Buf, BufferAlloc, ContiguousBuffer, GrowBufferMut, NoReuseBufferProvider,
    ReusableBuffer, Serializer,
};

use crate::{
    device::{
        queue::{fifo, DequeueState, EnqueueResult, TransmitQueueFrameError},
        socket::{DeviceSocketHandler, ParseSentFrameError, SentFrame},
        Device, DeviceIdContext, DeviceSendFrameError,
    },
    sync::Mutex,
};

#[derive(Derivative)]
#[derivative(Default(bound = "Allocator: Default"))]
pub(crate) struct TransmitQueueState<Meta, Buffer, Allocator> {
    pub(super) allocator: Allocator,
    pub(super) queue: Option<fifo::Queue<Meta, Buffer>>,
}

#[derive(Derivative)]
#[derivative(Default(bound = "Allocator: Default"))]
pub(crate) struct TransmitQueue<Meta, Buffer, Allocator> {
    /// The state for dequeued packets that will be handled.
    ///
    /// See `queue` for lock ordering.
    pub(crate) deque: Mutex<DequeueState<Meta, Buffer>>,
    /// A queue of to-be-transmitted packets protected by a lock.
    ///
    /// Lock ordering: `deque` must be locked before `queue` is locked when both
    /// are needed at the same time.
    pub(crate) queue: Mutex<TransmitQueueState<Meta, Buffer, Allocator>>,
}

/// The bindings context for the transmit queue.
pub trait TransmitQueueBindingsContext<D: Device, DeviceId> {
    /// Wakes up TX task.
    fn wake_tx_task(&mut self, device_id: &DeviceId);
}

pub trait TransmitQueueCommon<D: Device, C>: DeviceIdContext<D> {
    type Meta;
    type Allocator;
    type Buffer: GrowBufferMut + ContiguousBuffer;

    /// Parses an outgoing frame for packet socket delivery.
    fn parse_outgoing_frame<'a, 'b>(
        buf: &'a [u8],
        meta: &'a Self::Meta,
    ) -> Result<SentFrame<&'a [u8]>, ParseSentFrameError>;
}

/// The execution context for a transmit queue.
pub trait TransmitQueueContext<D: Device, BC>: TransmitQueueCommon<D, BC> {
    /// Returns the queue state, mutably.
    fn with_transmit_queue_mut<
        O,
        F: FnOnce(&mut TransmitQueueState<Self::Meta, Self::Buffer, Self::Allocator>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;

    /// Send a frame out the device.
    ///
    /// This method may not block - if the device is not ready, an appropriate
    /// error must be returned.
    fn send_frame(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        meta: Self::Meta,
        buf: Self::Buffer,
    ) -> Result<(), DeviceSendFrameError<(Self::Meta, Self::Buffer)>>;
}

pub trait TransmitDequeueContext<D: Device, BC>: TransmitQueueCommon<D, BC> {
    type TransmitQueueCtx<'a>: TransmitQueueContext<
            D,
            BC,
            Meta = Self::Meta,
            Buffer = Self::Buffer,
            DeviceId = Self::DeviceId,
        > + DeviceSocketHandler<D, BC>;

    /// Calls the function with the TX deque state and the TX queue context.
    fn with_dequed_packets_and_tx_queue_ctx<
        O,
        F: FnOnce(&mut DequeueState<Self::Meta, Self::Buffer>, &mut Self::TransmitQueueCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// The configuration for a transmit queue.
pub enum TransmitQueueConfiguration {
    /// No queue.
    None,
    /// FiFo queue.
    Fifo,
}

/// An implementation of a transmit queue that stores egress frames.
pub(crate) trait TransmitQueueHandler<D: Device, BC>: TransmitQueueCommon<D, BC> {
    /// Queues a frame for transmission.
    fn queue_tx_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        meta: Self::Meta,
        body: S,
    ) -> Result<(), TransmitQueueFrameError<S>>
    where
        S: Serializer,
        S::Buffer: ReusableBuffer;
}

pub(super) fn deliver_to_device_sockets<
    D: Device,
    BC: TransmitQueueBindingsContext<D, CC::DeviceId>,
    CC: TransmitQueueCommon<D, BC> + DeviceSocketHandler<D, BC>,
>(
    core_ctx: &mut CC,
    bindings_ctx: &mut BC,
    device_id: &CC::DeviceId,
    buffer: &CC::Buffer,
    meta: &CC::Meta,
) {
    let bytes = buffer.as_ref();
    match CC::parse_outgoing_frame(bytes, meta) {
        Ok(sent_frame) => DeviceSocketHandler::handle_frame(
            core_ctx,
            bindings_ctx,
            device_id,
            sent_frame.into(),
            bytes,
        ),
        Err(ParseSentFrameError) => {
            tracing::trace!(
                "failed to parse outgoing frame on {:?} ({} bytes)",
                device_id,
                bytes.len()
            )
        }
    }
}

impl<
        D: Device,
        BC: TransmitQueueBindingsContext<D, CC::DeviceId>,
        CC: TransmitQueueContext<D, BC> + DeviceSocketHandler<D, BC>,
    > TransmitQueueHandler<D, BC> for CC
where
    for<'a> &'a mut CC::Allocator: BufferAlloc<CC::Buffer>,
    CC::Buffer: ReusableBuffer,
{
    fn queue_tx_frame<S>(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &CC::DeviceId,
        meta: CC::Meta,
        body: S,
    ) -> Result<(), TransmitQueueFrameError<S>>
    where
        S: Serializer,
        S::Buffer: ReusableBuffer,
    {
        enum EnqueueStatus<N> {
            NotAttempted(N),
            Attempted,
        }

        let result =
            self.with_transmit_queue_mut(device_id, |TransmitQueueState { allocator, queue }| {
                let get_buffer = |body: S| {
                    body.serialize_outer(NoReuseBufferProvider(allocator))
                        .map_err(|(_e, s)| TransmitQueueFrameError::SerializeError(s))
                };

                match queue {
                    // No TX queue so send the frame immediately.
                    None => get_buffer(body).map(|buf| EnqueueStatus::NotAttempted((buf, meta))),
                    Some(queue) => queue.queue_tx_frame(meta, body, get_buffer).map(|res| {
                        match res {
                            EnqueueResult::QueueWasPreviouslyEmpty => {
                                bindings_ctx.wake_tx_task(device_id);
                            }
                            EnqueueResult::QueuePreviouslyWasOccupied => {}
                        }

                        EnqueueStatus::Attempted
                    }),
                }
            })?;

        match result {
            EnqueueStatus::NotAttempted((body, meta)) => {
                // TODO(https://fxbug.dev/42077654): Deliver the frame to packet
                // sockets and to the device atomically.
                deliver_to_device_sockets(self, bindings_ctx, device_id, &body, &meta);
                // Send the frame while not holding the TX queue exclusively to
                // not block concurrent senders from making progress.
                self.send_frame(bindings_ctx, device_id, meta, body).map_err(|_| {
                    TransmitQueueFrameError::NoQueue(DeviceSendFrameError::DeviceNotReady(()))
                })
            }
            EnqueueStatus::Attempted => Ok(()),
        }
    }
}

#[derive(Default)]
pub struct BufVecU8Allocator;

impl<'a> BufferAlloc<Buf<Vec<u8>>> for &'a mut BufVecU8Allocator {
    type Error = Never;

    fn alloc(self, len: usize) -> Result<Buf<Vec<u8>>, Self::Error> {
        new_buf_vec(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::vec;

    use net_declare::net_mac;
    use net_types::ethernet::Mac;
    use test_case::test_case;

    use crate::{
        context::testutil::{FakeBindingsCtx, FakeCoreCtx, FakeCtx},
        device::{
            link::testutil::{FakeLinkDevice, FakeLinkDeviceId},
            queue::{api::TransmitQueueApi, MAX_BATCH_SIZE, MAX_TX_QUEUED_LEN},
            socket::{EthernetFrame, Frame},
        },
        work_queue::WorkQueueReport,
    };

    #[derive(Default)]
    struct FakeTxQueueState {
        queue: TransmitQueueState<(), Buf<Vec<u8>>, BufVecU8Allocator>,
        transmitted_packets: Vec<Buf<Vec<u8>>>,
        device_not_ready: bool,
    }

    #[derive(Default)]
    struct FakeTxQueueBindingsCtxState {
        woken_tx_tasks: Vec<FakeLinkDeviceId>,
        delivered_to_sockets: Vec<Frame<Vec<u8>>>,
    }

    type FakeCoreCtxImpl = FakeCoreCtx<FakeTxQueueState, (), FakeLinkDeviceId>;
    type FakeBindingsCtxImpl = FakeBindingsCtx<(), (), FakeTxQueueBindingsCtxState, ()>;

    impl TransmitQueueBindingsContext<FakeLinkDevice, FakeLinkDeviceId> for FakeBindingsCtxImpl {
        fn wake_tx_task(&mut self, device_id: &FakeLinkDeviceId) {
            self.state.woken_tx_tasks.push(device_id.clone())
        }
    }

    const SRC_MAC: Mac = net_mac!("AA:BB:CC:DD:EE:FF");
    const DEST_MAC: Mac = net_mac!("FF:EE:DD:CC:BB:AA");

    impl TransmitQueueCommon<FakeLinkDevice, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        type Meta = ();
        type Buffer = Buf<Vec<u8>>;
        type Allocator = BufVecU8Allocator;

        fn parse_outgoing_frame<'a, 'b>(
            buf: &'a [u8],
            (): &'b Self::Meta,
        ) -> Result<SentFrame<&'a [u8]>, ParseSentFrameError> {
            Ok(fake_sent_ethernet_with_body(buf))
        }
    }

    fn fake_sent_ethernet_with_body<B>(body: B) -> SentFrame<B> {
        SentFrame::Ethernet(EthernetFrame {
            src_mac: SRC_MAC,
            dst_mac: DEST_MAC,
            ethertype: None,
            body,
        })
    }

    /// A trait providing a shortcut to instantiate a [`TransmitQueueApi`] from a context.
    trait TransmitQueueApiExt: crate::context::ContextPair + Sized {
        fn transmit_queue_api<D>(&mut self) -> TransmitQueueApi<D, &mut Self> {
            TransmitQueueApi::new(self)
        }
    }

    impl<O> TransmitQueueApiExt for O where O: crate::context::ContextPair + Sized {}

    impl TransmitQueueContext<FakeLinkDevice, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        fn with_transmit_queue_mut<
            O,
            F: FnOnce(&mut TransmitQueueState<(), Buf<Vec<u8>>, BufVecU8Allocator>) -> O,
        >(
            &mut self,
            &FakeLinkDeviceId: &FakeLinkDeviceId,
            cb: F,
        ) -> O {
            cb(&mut self.state.queue)
        }

        fn send_frame(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtxImpl,
            &FakeLinkDeviceId: &FakeLinkDeviceId,
            meta: (),
            buf: Buf<Vec<u8>>,
        ) -> Result<(), DeviceSendFrameError<(Self::Meta, Self::Buffer)>> {
            let FakeTxQueueState { queue: _, transmitted_packets, device_not_ready } =
                &mut self.state;
            if *device_not_ready {
                Err(DeviceSendFrameError::DeviceNotReady((meta, buf)))
            } else {
                Ok(transmitted_packets.push(buf))
            }
        }
    }

    impl TransmitDequeueContext<FakeLinkDevice, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        type TransmitQueueCtx<'a> = Self;

        fn with_dequed_packets_and_tx_queue_ctx<
            O,
            F: FnOnce(
                &mut DequeueState<Self::Meta, Self::Buffer>,
                &mut Self::TransmitQueueCtx<'_>,
            ) -> O,
        >(
            &mut self,
            &FakeLinkDeviceId: &FakeLinkDeviceId,
            cb: F,
        ) -> O {
            cb(&mut DequeueState::default(), self)
        }
    }

    impl DeviceSocketHandler<FakeLinkDevice, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        fn handle_frame(
            &mut self,
            bindings_ctx: &mut FakeBindingsCtxImpl,
            _device: &Self::DeviceId,
            frame: Frame<&[u8]>,
            _whole_frame: &[u8],
        ) {
            bindings_ctx.state.delivered_to_sockets.push(frame.cloned())
        }
    }

    #[test]
    fn noqueue() {
        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        let body = Buf::new(vec![0], ..);

        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        assert_eq!(
            TransmitQueueHandler::queue_tx_frame(
                core_ctx,
                bindings_ctx,
                &FakeLinkDeviceId,
                (),
                body.clone(),
            ),
            Ok(())
        );
        let FakeTxQueueBindingsCtxState { woken_tx_tasks, delivered_to_sockets } =
            &bindings_ctx.state;
        assert_eq!(woken_tx_tasks, &[]);
        assert_eq!(
            delivered_to_sockets,
            &[Frame::Sent(fake_sent_ethernet_with_body(body.as_ref().into()))]
        );
        assert_eq!(core::mem::take(&mut core_ctx.state.transmitted_packets), [body]);

        // Should not have any frames waiting to be transmitted since we have no
        // queue.
        assert_eq!(
            ctx.transmit_queue_api().transmit_queued_frames(&FakeLinkDeviceId),
            Ok(WorkQueueReport::AllDone),
        );

        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        assert_eq!(bindings_ctx.state.woken_tx_tasks, []);
        assert_eq!(core::mem::take(&mut core_ctx.state.transmitted_packets), []);
    }

    #[test]
    fn fifo_queue_and_dequeue() {
        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        ctx.transmit_queue_api()
            .set_configuration(&FakeLinkDeviceId, TransmitQueueConfiguration::Fifo);

        for _ in 0..2 {
            let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
            for i in 0..MAX_TX_QUEUED_LEN {
                let body = Buf::new(vec![i as u8], ..);
                assert_eq!(
                    TransmitQueueHandler::queue_tx_frame(
                        core_ctx,
                        bindings_ctx,
                        &FakeLinkDeviceId,
                        (),
                        body
                    ),
                    Ok(())
                );
                // We should only ever be woken up once when the first packet
                // was enqueued.
                assert_eq!(bindings_ctx.state.woken_tx_tasks, [FakeLinkDeviceId]);
            }

            let body = Buf::new(vec![131], ..);
            assert_eq!(
                TransmitQueueHandler::queue_tx_frame(
                    core_ctx,
                    bindings_ctx,
                    &FakeLinkDeviceId,
                    (),
                    body.clone(),
                ),
                Err(TransmitQueueFrameError::QueueFull(body))
            );

            let FakeTxQueueBindingsCtxState { woken_tx_tasks, delivered_to_sockets } =
                &mut bindings_ctx.state;
            // We should only ever be woken up once when the first packet
            // was enqueued.
            assert_eq!(core::mem::take(woken_tx_tasks), [FakeLinkDeviceId]);
            // No frames should be delivered to packet sockets before transmit.
            assert_eq!(core::mem::take(delivered_to_sockets), &[]);

            assert!(MAX_TX_QUEUED_LEN > MAX_BATCH_SIZE);
            for i in (0..(MAX_TX_QUEUED_LEN - MAX_BATCH_SIZE)).step_by(MAX_BATCH_SIZE) {
                assert_eq!(
                    ctx.transmit_queue_api().transmit_queued_frames(&FakeLinkDeviceId),
                    Ok(WorkQueueReport::Pending),
                );
                assert_eq!(
                    core::mem::take(&mut ctx.core_ctx.state.transmitted_packets),
                    (i..i + MAX_BATCH_SIZE)
                        .map(|i| Buf::new(vec![i as u8], ..))
                        .collect::<Vec<_>>()
                );
            }

            assert_eq!(
                ctx.transmit_queue_api().transmit_queued_frames(&FakeLinkDeviceId),
                Ok(WorkQueueReport::AllDone),
            );

            let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
            assert_eq!(
                core::mem::take(&mut core_ctx.state.transmitted_packets),
                (MAX_BATCH_SIZE * (MAX_TX_QUEUED_LEN / MAX_BATCH_SIZE - 1)..MAX_TX_QUEUED_LEN)
                    .map(|i| Buf::new(vec![i as u8], ..))
                    .collect::<Vec<_>>()
            );
            // Should not have woken up the TX task since the queue should be
            // empty.
            let FakeTxQueueBindingsCtxState { woken_tx_tasks, delivered_to_sockets } =
                &mut bindings_ctx.state;
            assert_eq!(core::mem::take(woken_tx_tasks), []);

            // The queue should now be empty so the next iteration of queueing
            // `MAX_TX_QUEUED_FRAMES` packets should succeed.
            assert_eq!(
                core::mem::take(delivered_to_sockets),
                (0..MAX_TX_QUEUED_LEN)
                    .map(|i| Frame::Sent(fake_sent_ethernet_with_body(vec![i as u8])))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn device_not_ready() {
        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        ctx.transmit_queue_api()
            .set_configuration(&FakeLinkDeviceId, TransmitQueueConfiguration::Fifo);

        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        let body = Buf::new(vec![0], ..);
        assert_eq!(
            TransmitQueueHandler::queue_tx_frame(
                core_ctx,
                bindings_ctx,
                &FakeLinkDeviceId,
                (),
                body.clone(),
            ),
            Ok(())
        );
        assert_eq!(core::mem::take(&mut bindings_ctx.state.woken_tx_tasks), [FakeLinkDeviceId]);
        assert_eq!(core_ctx.state.transmitted_packets, []);

        core_ctx.state.device_not_ready = true;
        assert_eq!(
            ctx.transmit_queue_api().transmit_queued_frames(&FakeLinkDeviceId,),
            Err(DeviceSendFrameError::DeviceNotReady(())),
        );
        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        assert_eq!(core_ctx.state.transmitted_packets, []);
        let FakeTxQueueBindingsCtxState { woken_tx_tasks, delivered_to_sockets } =
            &bindings_ctx.state;
        assert_eq!(woken_tx_tasks, &[]);
        // Frames were delivered to packet sockets before the device was found
        // to not be ready.
        assert_eq!(
            delivered_to_sockets,
            &[Frame::Sent(fake_sent_ethernet_with_body(body.as_ref().into()))]
        );

        core_ctx.state.device_not_ready = false;
        assert_eq!(
            ctx.transmit_queue_api().transmit_queued_frames(&FakeLinkDeviceId,),
            Ok(WorkQueueReport::AllDone),
        );
        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        assert_eq!(bindings_ctx.state.woken_tx_tasks, []);
        assert_eq!(core::mem::take(&mut core_ctx.state.transmitted_packets), [body]);
    }

    #[test_case(true; "device not ready")]
    #[test_case(false; "device ready")]
    fn drain_before_noqueue(device_not_ready: bool) {
        let mut ctx = FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        ctx.transmit_queue_api()
            .set_configuration(&FakeLinkDeviceId, TransmitQueueConfiguration::Fifo);

        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        let body = Buf::new(vec![0], ..);
        assert_eq!(
            TransmitQueueHandler::queue_tx_frame(
                core_ctx,
                bindings_ctx,
                &FakeLinkDeviceId,
                (),
                body.clone(),
            ),
            Ok(())
        );
        assert_eq!(core::mem::take(&mut bindings_ctx.state.woken_tx_tasks), [FakeLinkDeviceId]);
        assert_eq!(core_ctx.state.transmitted_packets, []);

        core_ctx.state.device_not_ready = device_not_ready;
        ctx.transmit_queue_api()
            .set_configuration(&FakeLinkDeviceId, TransmitQueueConfiguration::None);

        let FakeCtx { core_ctx, bindings_ctx } = &mut ctx;
        let FakeTxQueueBindingsCtxState { woken_tx_tasks, delivered_to_sockets } =
            &bindings_ctx.state;
        assert_eq!(woken_tx_tasks, &[]);
        assert_eq!(
            delivered_to_sockets,
            &[Frame::Sent(fake_sent_ethernet_with_body(body.as_ref().into()))]
        );
        if device_not_ready {
            assert_eq!(core_ctx.state.transmitted_packets, []);
        } else {
            assert_eq!(core::mem::take(&mut core_ctx.state.transmitted_packets), [body]);
        }
    }
}
