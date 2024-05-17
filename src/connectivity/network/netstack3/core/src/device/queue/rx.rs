// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! RX device queues.

use core::fmt::Debug;

#[cfg(test)]
use assert_matches::assert_matches;
use derivative::Derivative;
use packet::BufferMut;

use crate::{
    device::{
        queue::{fifo, DequeueState, EnqueueResult, ReceiveQueueFullError},
        Device, DeviceIdContext,
    },
    sync::Mutex,
};

/// The state used to hold a queue of received frames to be handled at a later
/// time.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct ReceiveQueueState<Meta, Buffer> {
    pub(super) queue: fifo::Queue<Meta, Buffer>,
}

impl<Meta, Buffer> ReceiveQueueState<Meta, Buffer> {
    #[cfg(test)]
    pub(crate) fn take_frames(&mut self) -> impl Iterator<Item = (Meta, Buffer)> + '_ {
        let Self { queue } = self;
        let mut vec = Default::default();
        assert_matches!(
            queue.dequeue_into(&mut vec, usize::MAX),
            crate::device::queue::DequeueResult::NoMoreLeft
        );
        vec.into_iter()
    }
}

/// The bindings context for the receive queue.
pub trait ReceiveQueueBindingsContext<DeviceId> {
    /// Signals to bindings that RX frames are available and ready to be handled
    /// by device.
    ///
    /// Implementations must make sure that the API call to handle queued
    /// packets is scheduled to be called as soon as possible so that enqueued
    /// RX frames are promptly handled.
    fn wake_rx_task(&mut self, device_id: &DeviceId);
}

#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub(crate) struct ReceiveQueue<Meta, Buffer> {
    /// The state for dequeued frames that will be handled.
    ///
    /// See `queue` for lock ordering.
    pub(crate) deque: Mutex<DequeueState<Meta, Buffer>>,
    /// A queue of received frames protected by a lock.
    ///
    /// Lock ordering: `deque` must be locked before `queue` is locked when both
    /// are needed at the same time.
    pub(crate) queue: Mutex<ReceiveQueueState<Meta, Buffer>>,
}

pub trait ReceiveQueueTypes<D: Device, BC>: DeviceIdContext<D> {
    /// Metadata associated with an RX frame.
    type Meta;

    /// The type of buffer holding an RX frame.
    type Buffer: BufferMut + Debug;
}

/// The execution context for a receive queue.
pub trait ReceiveQueueContext<D: Device, BC>: ReceiveQueueTypes<D, BC> {
    /// Calls the function with the RX queue state.
    fn with_receive_queue_mut<O, F: FnOnce(&mut ReceiveQueueState<Self::Meta, Self::Buffer>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

pub trait ReceiveDequeFrameContext<D: Device, BC>: ReceiveQueueTypes<D, BC> {
    /// Handle a received frame.
    fn handle_frame(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        meta: Self::Meta,
        buf: Self::Buffer,
    );
}

pub trait ReceiveDequeContext<D: Device, BC>: ReceiveQueueTypes<D, BC> {
    type ReceiveQueueCtx<'a>: ReceiveQueueContext<
            D,
            BC,
            Meta = Self::Meta,
            Buffer = Self::Buffer,
            DeviceId = Self::DeviceId,
        > + ReceiveDequeFrameContext<
            D,
            BC,
            Meta = Self::Meta,
            Buffer = Self::Buffer,
            DeviceId = Self::DeviceId,
        >;

    /// Calls the function with the RX deque state and the RX queue context.
    fn with_dequed_frames_and_rx_queue_ctx<
        O,
        F: FnOnce(&mut DequeueState<Self::Meta, Self::Buffer>, &mut Self::ReceiveQueueCtx<'_>) -> O,
    >(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

/// An implementation of a receive queue, with a buffer.
pub(crate) trait ReceiveQueueHandler<D: Device, BC>: ReceiveQueueTypes<D, BC> {
    /// Queues a frame for reception.
    ///
    /// # Errors
    ///
    /// Returns an error with the metadata and body if the queue is full.
    fn queue_rx_frame(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        meta: Self::Meta,
        body: Self::Buffer,
    ) -> Result<(), ReceiveQueueFullError<(Self::Meta, Self::Buffer)>>;
}

impl<D: Device, BC: ReceiveQueueBindingsContext<CC::DeviceId>, CC: ReceiveQueueContext<D, BC>>
    ReceiveQueueHandler<D, BC> for CC
{
    fn queue_rx_frame(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &CC::DeviceId,
        meta: CC::Meta,
        body: CC::Buffer,
    ) -> Result<(), ReceiveQueueFullError<(Self::Meta, CC::Buffer)>> {
        self.with_receive_queue_mut(device_id, |ReceiveQueueState { queue }| {
            queue.queue_rx_frame(meta, body).map(|res| match res {
                EnqueueResult::QueueWasPreviouslyEmpty => bindings_ctx.wake_rx_task(device_id),
                EnqueueResult::QueuePreviouslyWasOccupied => {
                    // We have already woken up the RX task when the queue was
                    // previously empty so there is no need to do it again.
                }
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::{vec, vec::Vec};

    use packet::Buf;

    use crate::{
        context::{
            testutil::{FakeBindingsCtx, FakeCoreCtx},
            CtxPair,
        },
        device::{
            link::testutil::{FakeLinkDevice, FakeLinkDeviceId},
            queue::{api::ReceiveQueueApi, MAX_BATCH_SIZE, MAX_RX_QUEUED_LEN},
        },
        types::WorkQueueReport,
    };

    #[derive(Default)]
    struct FakeRxQueueState {
        queue: ReceiveQueueState<(), Buf<Vec<u8>>>,
        handled_frames: Vec<Buf<Vec<u8>>>,
    }

    #[derive(Default)]
    struct FakeRxQueueBindingsCtxState {
        woken_rx_tasks: Vec<FakeLinkDeviceId>,
    }

    type FakeCoreCtxImpl = FakeCoreCtx<FakeRxQueueState, (), FakeLinkDeviceId>;
    type FakeBindingsCtxImpl = FakeBindingsCtx<(), (), FakeRxQueueBindingsCtxState, ()>;

    impl ReceiveQueueBindingsContext<FakeLinkDeviceId> for FakeBindingsCtxImpl {
        fn wake_rx_task(&mut self, device_id: &FakeLinkDeviceId) {
            self.state.woken_rx_tasks.push(device_id.clone())
        }
    }

    impl ReceiveQueueTypes<FakeLinkDevice, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        type Meta = ();
        type Buffer = Buf<Vec<u8>>;
    }

    impl ReceiveQueueContext<FakeLinkDevice, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        fn with_receive_queue_mut<O, F: FnOnce(&mut ReceiveQueueState<(), Buf<Vec<u8>>>) -> O>(
            &mut self,
            &FakeLinkDeviceId: &FakeLinkDeviceId,
            cb: F,
        ) -> O {
            cb(&mut self.state.queue)
        }
    }

    impl ReceiveDequeFrameContext<FakeLinkDevice, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        fn handle_frame(
            &mut self,
            _bindings_ctx: &mut FakeBindingsCtxImpl,
            &FakeLinkDeviceId: &FakeLinkDeviceId,
            (): (),
            buf: Buf<Vec<u8>>,
        ) {
            self.state.handled_frames.push(buf)
        }
    }

    impl ReceiveDequeContext<FakeLinkDevice, FakeBindingsCtxImpl> for FakeCoreCtxImpl {
        type ReceiveQueueCtx<'a> = Self;

        fn with_dequed_frames_and_rx_queue_ctx<
            O,
            F: FnOnce(
                &mut DequeueState<Self::Meta, Self::Buffer>,
                &mut Self::ReceiveQueueCtx<'_>,
            ) -> O,
        >(
            &mut self,
            &FakeLinkDeviceId: &FakeLinkDeviceId,
            cb: F,
        ) -> O {
            cb(&mut DequeueState::default(), self)
        }
    }

    /// A trait providing a shortcut to instantiate a [`TransmitQueueApi`] from a context.
    trait ReceiveQueueApiExt: crate::context::ContextPair + Sized {
        fn receive_queue_api<D>(&mut self) -> ReceiveQueueApi<D, &mut Self> {
            ReceiveQueueApi::new(self)
        }
    }

    impl<O> ReceiveQueueApiExt for O where O: crate::context::ContextPair + Sized {}

    #[test]
    fn queue_and_dequeue() {
        let mut ctx = CtxPair::with_core_ctx(FakeCoreCtxImpl::default());

        for _ in 0..2 {
            let CtxPair { core_ctx, bindings_ctx } = &mut ctx;
            for i in 0..MAX_RX_QUEUED_LEN {
                let body = Buf::new(vec![i as u8], ..);
                assert_eq!(
                    ReceiveQueueHandler::queue_rx_frame(
                        core_ctx,
                        bindings_ctx,
                        &FakeLinkDeviceId,
                        (),
                        body
                    ),
                    Ok(())
                );
                // We should only ever be woken up once when the first frame
                // was enqueued.
                assert_eq!(bindings_ctx.state.woken_rx_tasks, [FakeLinkDeviceId]);
            }

            let body = Buf::new(vec![131], ..);
            assert_eq!(
                ReceiveQueueHandler::queue_rx_frame(
                    core_ctx,
                    bindings_ctx,
                    &FakeLinkDeviceId,
                    (),
                    body.clone(),
                ),
                Err(ReceiveQueueFullError(((), body)))
            );
            // We should only ever be woken up once when the first frame
            // was enqueued.
            assert_eq!(core::mem::take(&mut bindings_ctx.state.woken_rx_tasks), [FakeLinkDeviceId]);

            assert!(MAX_RX_QUEUED_LEN > MAX_BATCH_SIZE);
            for i in (0..(MAX_RX_QUEUED_LEN - MAX_BATCH_SIZE)).step_by(MAX_BATCH_SIZE) {
                assert_eq!(
                    ctx.receive_queue_api().handle_queued_frames(&FakeLinkDeviceId),
                    WorkQueueReport::Pending
                );
                assert_eq!(
                    core::mem::take(&mut ctx.core_ctx.state.handled_frames),
                    (i..i + MAX_BATCH_SIZE)
                        .map(|i| Buf::new(vec![i as u8], ..))
                        .collect::<Vec<_>>()
                );
            }

            assert_eq!(
                ctx.receive_queue_api().handle_queued_frames(&FakeLinkDeviceId),
                WorkQueueReport::AllDone
            );
            let CtxPair { core_ctx, bindings_ctx } = &mut ctx;
            assert_eq!(
                core::mem::take(&mut core_ctx.state.handled_frames),
                (MAX_BATCH_SIZE * (MAX_RX_QUEUED_LEN / MAX_BATCH_SIZE - 1)..MAX_RX_QUEUED_LEN)
                    .map(|i| Buf::new(vec![i as u8], ..))
                    .collect::<Vec<_>>()
            );
            // Should not have woken up the RX task since the queue should be
            // empty.
            assert_eq!(core::mem::take(&mut bindings_ctx.state.woken_rx_tasks), []);

            // The queue should now be empty so the next iteration of queueing
            // `MAX_RX_QUEUED_LEN` frames should succeed.
        }
    }
}
