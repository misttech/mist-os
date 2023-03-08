// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! TX device queues.

use alloc::vec::Vec;
use core::convert::Infallible as Never;

use derivative::Derivative;
use packet::{
    new_buf_vec, Buf, BufferProvider, FragmentedBufferMut, ReusableBuffer, Serializer,
    ShrinkBuffer, TargetBuffer,
};

use crate::{
    device::{
        queue::{
            fifo, DequeueResult, DequeueState, EnqueueResult, TransmitQueueFrameError,
            MAX_BATCH_SIZE,
        },
        Device, DeviceIdContext, DeviceSendFrameError,
    },
    sync::Mutex,
};

#[derive(Derivative)]
#[derivative(Default(bound = "Allocator: Default"))]
pub(crate) struct TransmitQueueState<Meta, Buffer, Allocator> {
    allocator: Allocator,
    queue: Option<fifo::Queue<Meta, Buffer>>,
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

/// The non-synchonized context for the transmit queue.
pub(crate) trait TransmitQueueNonSyncContext<D: Device, DeviceId> {
    /// Wakes up TX task.
    fn wake_tx_task(&mut self, device_id: &DeviceId);
}

pub(crate) trait TransmitQueueTypes<D: Device, C>: DeviceIdContext<D> {
    type Meta;
    type Allocator;
    type Buffer: TargetBuffer;
}

/// The execution context for a transmit queue.
pub(crate) trait TransmitQueueContext<D: Device, C>: TransmitQueueTypes<D, C> {
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
        ctx: &mut C,
        device_id: &Self::DeviceId,
        meta: Self::Meta,
        buf: Self::Buffer,
    ) -> Result<(), DeviceSendFrameError<(Self::Meta, Self::Buffer)>>;
}

pub(crate) trait TransmitDequeueContext<D: Device, C>: TransmitQueueTypes<D, C> {
    type TransmitQueueCtx<'a>: TransmitQueueContext<
        D,
        C,
        Meta = Self::Meta,
        Buffer = Self::Buffer,
        DeviceId = Self::DeviceId,
    >;

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

/// An implementation of a transmit queue.
pub(crate) trait TransmitQueueHandler<D: Device, C>: TransmitQueueTypes<D, C> {
    /// Transmits any queued frames.
    fn transmit_queued_frames(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
    ) -> Result<(), DeviceSendFrameError<()>>;

    /// Sets the queue configuration for the device.
    fn set_configuration(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        config: TransmitQueueConfiguration,
    );
}

/// An implementation of a transmit queue, with a buffer.
pub(crate) trait BufferTransmitQueueHandler<D: Device, I: ReusableBuffer, C>:
    TransmitQueueTypes<D, C>
{
    /// Queues a frame for transmission.
    fn queue_tx_frame<S: Serializer<Buffer = I>>(
        &mut self,
        ctx: &mut C,
        device_id: &Self::DeviceId,
        meta: Self::Meta,
        body: S,
    ) -> Result<(), TransmitQueueFrameError<S>>;
}

impl<
        D: Device,
        C: TransmitQueueNonSyncContext<D, SC::DeviceId>,
        SC: TransmitDequeueContext<D, C>,
    > TransmitQueueHandler<D, C> for SC
{
    fn transmit_queued_frames(
        &mut self,
        ctx: &mut C,
        device_id: &SC::DeviceId,
    ) -> Result<(), DeviceSendFrameError<()>> {
        self.with_dequed_packets_and_tx_queue_ctx(
            device_id,
            |DequeueState { dequed_packets }, tx_queue_ctx| {
                assert!(
                    dequed_packets.is_empty(),
                    "should never have left packets after attempting to dequeue"
                );

                let ret = tx_queue_ctx.with_transmit_queue_mut(
                    device_id,
                    |TransmitQueueState { allocator: _, queue }| {
                        queue
                            .as_mut()
                            .map(|q| q.dequeue_packets_into(dequed_packets, MAX_BATCH_SIZE))
                    },
                );
                let Some(ret) = ret else { return Ok(()) };

                while let Some((meta, p)) = dequed_packets.pop_front() {
                    match tx_queue_ctx.send_frame(ctx, device_id, meta, p) {
                        Ok(()) => {}
                        Err(DeviceSendFrameError::DeviceNotReady(x)) => {
                            // We failed to send the frame so requeue it and try
                            // again later.
                            tx_queue_ctx.with_transmit_queue_mut(
                                device_id,
                                |TransmitQueueState { allocator: _, queue }| {
                                    dequed_packets.push_front(x);
                                    queue.as_mut().unwrap().requeue_packets(dequed_packets);
                                },
                            );
                            return Err(DeviceSendFrameError::DeviceNotReady(()));
                        }
                    }
                }

                match ret {
                    DequeueResult::MorePacketsStillQueued => ctx.wake_tx_task(device_id),
                    DequeueResult::NoMorePacketsLeft => {
                        // There are no more frames left after the batch we
                        // just handled. When the next TX frame gets enqueued,
                        // the TX task will be woken up again.
                    }
                }

                Ok(())
            },
        )
    }

    fn set_configuration(
        &mut self,
        ctx: &mut C,
        device_id: &SC::DeviceId,
        config: TransmitQueueConfiguration,
    ) {
        // We take the dequeue lock as well to make sure we finish any current
        // dequeuing before changing the configuration.
        self.with_dequed_packets_and_tx_queue_ctx(
            device_id,
            |DequeueState { dequed_packets }, tx_queue_ctx| {
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
                    let ret = prev_queue.dequeue_packets_into(dequed_packets, MAX_BATCH_SIZE);

                    while let Some((meta, p)) = dequed_packets.pop_front() {
                        match tx_queue_ctx.send_frame(ctx, device_id, meta, p) {
                            Ok(()) => {}
                            Err(DeviceSendFrameError::DeviceNotReady(x)) => {
                                // We swapped to no-queue and device cannot send
                                // the frame so we just drop it.
                                let _: (Self::Meta, Self::Buffer) = x;
                            }
                        }
                    }

                    match ret {
                        DequeueResult::NoMorePacketsLeft => break,
                        DequeueResult::MorePacketsStillQueued => {}
                    }
                }
            },
        )
    }
}

impl<
        D: Device,
        I: ReusableBuffer,
        C: TransmitQueueNonSyncContext<D, SC::DeviceId>,
        SC: TransmitQueueContext<D, C>,
    > BufferTransmitQueueHandler<D, I, C> for SC
where
    for<'a> &'a mut SC::Allocator: BufferProvider<I, SC::Buffer>,
{
    fn queue_tx_frame<S: Serializer<Buffer = I>>(
        &mut self,
        ctx: &mut C,
        device_id: &SC::DeviceId,
        meta: SC::Meta,
        body: S,
    ) -> Result<(), TransmitQueueFrameError<S>> {
        enum EnqueueStatus<N> {
            NotAttempted(N),
            Attempted,
        }

        let result =
            self.with_transmit_queue_mut(device_id, |TransmitQueueState { allocator, queue }| {
                let get_buffer = |body: S| {
                    body.serialize_outer(allocator)
                        .map_err(|(_e, s)| TransmitQueueFrameError::SerializeError(s))
                };

                match queue {
                    // No TX queue so send the frame immediately.
                    None => get_buffer(body).map(|buf| EnqueueStatus::NotAttempted((buf, meta))),
                    Some(queue) => queue.queue_tx_frame(meta, body, get_buffer).map(|res| {
                        match res {
                            EnqueueResult::QueueWasPreviouslyEmpty => {
                                ctx.wake_tx_task(device_id);
                            }
                            EnqueueResult::QueuePreviouslyHadPackets => {}
                        }

                        EnqueueStatus::Attempted
                    }),
                }
            })?;

        match result {
            EnqueueStatus::NotAttempted((body, meta)) => {
                // Send the frame while not holding the TX queue exclusively to
                // not block concurrent senders from making progress.
                self.send_frame(ctx, device_id, meta, body).map_err(|_| {
                    TransmitQueueFrameError::NoQueue(DeviceSendFrameError::DeviceNotReady(()))
                })
            }
            EnqueueStatus::Attempted => Ok(()),
        }
    }
}

#[derive(Default)]
pub(crate) struct BufVecU8Allocator;

impl<I: ReusableBuffer> BufferProvider<I, Buf<Vec<u8>>> for &'_ mut BufVecU8Allocator {
    type Error = Never;

    fn alloc_no_reuse(
        self,
        prefix: usize,
        body: usize,
        suffix: usize,
    ) -> Result<Buf<Vec<u8>>, Never> {
        new_buf_vec(prefix + body + suffix).map(|mut b| {
            b.shrink(prefix..prefix + body);
            b
        })
    }

    fn reuse_or_realloc(
        self,
        buffer: I,
        prefix: usize,
        suffix: usize,
    ) -> Result<Buf<Vec<u8>>, (Never, I)> {
        BufferProvider::<I, Buf<Vec<u8>>>::alloc_no_reuse(self, prefix, buffer.len(), suffix)
            .map(|mut b| {
                b.copy_from(&buffer);
                b
            })
            .map_err(|e| (e, buffer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::{vec, vec::Vec};

    use packet::Buf;
    use test_case::test_case;

    use crate::{
        context::testutil::{FakeCtx, FakeNonSyncCtx, FakeSyncCtx},
        device::{
            link::testutil::{FakeLinkDevice, FakeLinkDeviceId},
            queue::MAX_TX_QUEUED_FRAMES,
        },
    };

    #[derive(Default)]
    struct FakeTxQueueState {
        queue: TransmitQueueState<(), Buf<Vec<u8>>, BufVecU8Allocator>,
        transmitted_packets: Vec<Buf<Vec<u8>>>,
        device_not_ready: bool,
    }

    #[derive(Default)]
    struct FakeTxQueueNonSyncCtxState {
        woken_tx_tasks: Vec<FakeLinkDeviceId>,
    }

    type FakeSyncCtxImpl = FakeSyncCtx<FakeTxQueueState, (), FakeLinkDeviceId>;
    type FakeNonSyncCtxImpl = FakeNonSyncCtx<(), (), FakeTxQueueNonSyncCtxState>;

    impl TransmitQueueNonSyncContext<FakeLinkDevice, FakeLinkDeviceId> for FakeNonSyncCtxImpl {
        fn wake_tx_task(&mut self, device_id: &FakeLinkDeviceId) {
            self.state_mut().woken_tx_tasks.push(device_id.clone())
        }
    }

    impl TransmitQueueTypes<FakeLinkDevice, FakeNonSyncCtxImpl> for FakeSyncCtxImpl {
        type Meta = ();
        type Buffer = Buf<Vec<u8>>;
        type Allocator = BufVecU8Allocator;
    }

    impl TransmitQueueContext<FakeLinkDevice, FakeNonSyncCtxImpl> for FakeSyncCtxImpl {
        fn with_transmit_queue_mut<
            O,
            F: FnOnce(&mut TransmitQueueState<(), Buf<Vec<u8>>, BufVecU8Allocator>) -> O,
        >(
            &mut self,
            &FakeLinkDeviceId: &FakeLinkDeviceId,
            cb: F,
        ) -> O {
            let FakeTxQueueState { queue, transmitted_packets: _, device_not_ready: _ } =
                self.get_mut();
            cb(queue)
        }

        fn send_frame(
            &mut self,
            _ctx: &mut FakeNonSyncCtxImpl,
            &FakeLinkDeviceId: &FakeLinkDeviceId,
            meta: (),
            buf: Buf<Vec<u8>>,
        ) -> Result<(), DeviceSendFrameError<(Self::Meta, Self::Buffer)>> {
            let FakeTxQueueState { queue: _, transmitted_packets, device_not_ready } =
                self.get_mut();
            if *device_not_ready {
                Err(DeviceSendFrameError::DeviceNotReady((meta, buf)))
            } else {
                Ok(transmitted_packets.push(buf))
            }
        }
    }

    impl TransmitDequeueContext<FakeLinkDevice, FakeNonSyncCtxImpl> for FakeSyncCtxImpl {
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

    #[test]
    fn noqueue() {
        let FakeCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtx::with_sync_ctx(FakeSyncCtxImpl::default());

        let body = Buf::new(vec![0], ..);
        assert_eq!(
            BufferTransmitQueueHandler::queue_tx_frame(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                (),
                body.clone(),
            ),
            Ok(())
        );
        assert_eq!(non_sync_ctx.state().woken_tx_tasks, []);
        assert_eq!(core::mem::take(&mut sync_ctx.get_mut().transmitted_packets), [body]);

        // Should not have any frames waiting to be transmitted since we have no
        // queue.
        assert_eq!(
            TransmitQueueHandler::transmit_queued_frames(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
            ),
            Ok(()),
        );
        assert_eq!(non_sync_ctx.state().woken_tx_tasks, []);
        assert_eq!(core::mem::take(&mut sync_ctx.get_mut().transmitted_packets), []);
    }

    #[test]
    fn fifo_queue_and_dequeue() {
        let FakeCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtx::with_sync_ctx(FakeSyncCtxImpl::default());

        TransmitQueueHandler::set_configuration(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            TransmitQueueConfiguration::Fifo,
        );

        for _ in 0..2 {
            for i in 0..MAX_TX_QUEUED_FRAMES {
                let body = Buf::new(vec![i as u8], ..);
                assert_eq!(
                    BufferTransmitQueueHandler::queue_tx_frame(
                        &mut sync_ctx,
                        &mut non_sync_ctx,
                        &FakeLinkDeviceId,
                        (),
                        body
                    ),
                    Ok(())
                );
                // We should only ever be woken up once when the first packet
                // was enqueued.
                assert_eq!(non_sync_ctx.state().woken_tx_tasks, [FakeLinkDeviceId]);
            }

            let body = Buf::new(vec![131], ..);
            assert_eq!(
                BufferTransmitQueueHandler::queue_tx_frame(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    &FakeLinkDeviceId,
                    (),
                    body.clone(),
                ),
                Err(TransmitQueueFrameError::QueueFull(body))
            );
            // We should only ever be woken up once when the first packet
            // was enqueued.
            assert_eq!(
                core::mem::take(&mut non_sync_ctx.state_mut().woken_tx_tasks),
                [FakeLinkDeviceId]
            );

            assert!(MAX_TX_QUEUED_FRAMES > MAX_BATCH_SIZE);
            for i in (0..(MAX_TX_QUEUED_FRAMES - MAX_BATCH_SIZE)).step_by(MAX_BATCH_SIZE) {
                assert_eq!(
                    TransmitQueueHandler::transmit_queued_frames(
                        &mut sync_ctx,
                        &mut non_sync_ctx,
                        &FakeLinkDeviceId,
                    ),
                    Ok(()),
                );
                assert_eq!(
                    core::mem::take(&mut sync_ctx.get_mut().transmitted_packets),
                    (i..i + MAX_BATCH_SIZE)
                        .into_iter()
                        .map(|i| Buf::new(vec![i as u8], ..))
                        .collect::<Vec<_>>()
                );
                // We should get a wake up signal when packets remain after
                // handling a batch of TX packets.
                assert_eq!(
                    core::mem::take(&mut non_sync_ctx.state_mut().woken_tx_tasks),
                    [FakeLinkDeviceId]
                );
            }

            assert_eq!(
                TransmitQueueHandler::transmit_queued_frames(
                    &mut sync_ctx,
                    &mut non_sync_ctx,
                    &FakeLinkDeviceId,
                ),
                Ok(()),
            );
            assert_eq!(
                core::mem::take(&mut sync_ctx.get_mut().transmitted_packets),
                (MAX_BATCH_SIZE * (MAX_TX_QUEUED_FRAMES / MAX_BATCH_SIZE - 1)
                    ..MAX_TX_QUEUED_FRAMES)
                    .into_iter()
                    .map(|i| Buf::new(vec![i as u8], ..))
                    .collect::<Vec<_>>()
            );
            // Should not have woken up the TX task since the queue should be
            // empty.
            assert_eq!(core::mem::take(&mut non_sync_ctx.state_mut().woken_tx_tasks), []);

            // The queue should now be empty so the next iteration of queueing
            // `MAX_TX_QUEUED_FRAMES` packets should succeed.
        }
    }

    #[test]
    fn device_not_ready() {
        let FakeCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtx::with_sync_ctx(FakeSyncCtxImpl::default());

        TransmitQueueHandler::set_configuration(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            TransmitQueueConfiguration::Fifo,
        );

        let body = Buf::new(vec![0], ..);
        assert_eq!(
            BufferTransmitQueueHandler::queue_tx_frame(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                (),
                body.clone(),
            ),
            Ok(())
        );
        assert_eq!(
            core::mem::take(&mut non_sync_ctx.state_mut().woken_tx_tasks),
            [FakeLinkDeviceId]
        );
        assert_eq!(sync_ctx.get_mut().transmitted_packets, []);

        sync_ctx.get_mut().device_not_ready = true;
        assert_eq!(
            TransmitQueueHandler::transmit_queued_frames(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
            ),
            Err(DeviceSendFrameError::DeviceNotReady(())),
        );
        assert_eq!(non_sync_ctx.state().woken_tx_tasks, []);
        assert_eq!(sync_ctx.get_mut().transmitted_packets, []);

        sync_ctx.get_mut().device_not_ready = false;
        assert_eq!(
            TransmitQueueHandler::transmit_queued_frames(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
            ),
            Ok(()),
        );
        assert_eq!(non_sync_ctx.state().woken_tx_tasks, []);
        assert_eq!(core::mem::take(&mut sync_ctx.get_mut().transmitted_packets), [body]);
    }

    #[test_case(true; "device not ready")]
    #[test_case(false; "device ready")]
    fn drain_before_noqueue(device_not_ready: bool) {
        let FakeCtx { mut sync_ctx, mut non_sync_ctx } =
            FakeCtx::with_sync_ctx(FakeSyncCtxImpl::default());

        TransmitQueueHandler::set_configuration(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            TransmitQueueConfiguration::Fifo,
        );

        let body = Buf::new(vec![0], ..);
        assert_eq!(
            BufferTransmitQueueHandler::queue_tx_frame(
                &mut sync_ctx,
                &mut non_sync_ctx,
                &FakeLinkDeviceId,
                (),
                body.clone(),
            ),
            Ok(())
        );
        assert_eq!(
            core::mem::take(&mut non_sync_ctx.state_mut().woken_tx_tasks),
            [FakeLinkDeviceId]
        );
        assert_eq!(sync_ctx.get_mut().transmitted_packets, []);

        sync_ctx.get_mut().device_not_ready = device_not_ready;
        TransmitQueueHandler::set_configuration(
            &mut sync_ctx,
            &mut non_sync_ctx,
            &FakeLinkDeviceId,
            TransmitQueueConfiguration::None,
        );
        assert_eq!(non_sync_ctx.state().woken_tx_tasks, []);
        if device_not_ready {
            assert_eq!(sync_ctx.get_mut().transmitted_packets, []);
        } else {
            assert_eq!(core::mem::take(&mut sync_ctx.get_mut().transmitted_packets), [body]);
        }
    }
}
