// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! RX device queues.

use core::convert::Infallible as Never;
use core::marker::PhantomData;

#[cfg(test)]
use assert_matches::assert_matches;
use derivative::Derivative;
use packet::ParseBuffer;

use crate::{
    device::{
        queue::{fifo, DequeueState, EnqueueResult, ReceiveQueueFullError, MAX_BATCH_SIZE},
        Device, DeviceIdContext,
    },
    sync::Mutex,
    work_queue::WorkQueueReport,
};

/// The state used to hold a queue of received frames to be handled at a later
/// time.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct ReceiveQueueState<Meta, Buffer> {
    queue: fifo::Queue<Meta, Buffer>,
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
pub(crate) trait ReceiveQueueBindingsContext<D: Device, DeviceId> {
    /// Wakes up RX task.
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

pub(crate) trait ReceiveQueueTypes<D: Device, BC>: DeviceIdContext<D> {
    /// Metadata associated with an RX frame.
    type Meta;

    /// The type of buffer holding an RX frame.
    type Buffer: ParseBuffer;
}

/// The execution context for a receive queue.
pub(crate) trait ReceiveQueueContext<D: Device, BC>: ReceiveQueueTypes<D, BC> {
    /// Calls the function with the RX queue state.
    fn with_receive_queue_mut<O, F: FnOnce(&mut ReceiveQueueState<Self::Meta, Self::Buffer>) -> O>(
        &mut self,
        device_id: &Self::DeviceId,
        cb: F,
    ) -> O;
}

pub(crate) trait ReceiveDequeFrameContext<D: Device, BC>: ReceiveQueueTypes<D, BC> {
    /// Handle a received frame.
    fn handle_frame(
        &mut self,
        bindings_ctx: &mut BC,
        device_id: &Self::DeviceId,
        meta: Self::Meta,
        buf: Self::Buffer,
    );
}

pub(crate) trait ReceiveDequeContext<D: Device, BC>: ReceiveQueueTypes<D, BC> {
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

/// Crate-internal receive queue API interaction.
pub(crate) struct ReceiveQueueApi<CC, BT, D>(Never, PhantomData<(CC, BT, D)>);

impl<CC, BC, D> ReceiveQueueApi<CC, BC, D>
where
    D: Device,
    BC: ReceiveQueueBindingsContext<D, CC::DeviceId>,
    CC: ReceiveDequeContext<D, BC>,
{
    /// Handle any queued RX frames.
    pub(crate) fn handle_queued_rx_frames(
        core_ctx: &mut CC,
        bindings_ctx: &mut BC,
        device_id: &CC::DeviceId,
    ) -> WorkQueueReport {
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

impl<
        D: Device,
        BC: ReceiveQueueBindingsContext<D, CC::DeviceId>,
        CC: ReceiveQueueContext<D, BC>,
    > ReceiveQueueHandler<D, BC> for CC
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
        context::testutil::{FakeBindingsCtx, FakeCoreCtx, FakeCtx},
        device::{
            link::testutil::{FakeLinkDevice, FakeLinkDeviceId},
            queue::MAX_RX_QUEUED_LEN,
        },
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
    type FakeBindingsCtxImpl = FakeBindingsCtx<(), (), FakeRxQueueBindingsCtxState>;

    impl ReceiveQueueBindingsContext<FakeLinkDevice, FakeLinkDeviceId> for FakeBindingsCtxImpl {
        fn wake_rx_task(&mut self, device_id: &FakeLinkDeviceId) {
            self.state_mut().woken_rx_tasks.push(device_id.clone())
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
            cb(&mut self.get_mut().queue)
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
            self.get_mut().handled_frames.push(buf)
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

    #[test]
    fn queue_and_dequeue() {
        let FakeCtx { mut core_ctx, mut bindings_ctx } =
            FakeCtx::with_core_ctx(FakeCoreCtxImpl::default());

        for _ in 0..2 {
            for i in 0..MAX_RX_QUEUED_LEN {
                let body = Buf::new(vec![i as u8], ..);
                assert_eq!(
                    ReceiveQueueHandler::queue_rx_frame(
                        &mut core_ctx,
                        &mut bindings_ctx,
                        &FakeLinkDeviceId,
                        (),
                        body
                    ),
                    Ok(())
                );
                // We should only ever be woken up once when the first frame
                // was enqueued.
                assert_eq!(bindings_ctx.state().woken_rx_tasks, [FakeLinkDeviceId]);
            }

            let body = Buf::new(vec![131], ..);
            assert_eq!(
                ReceiveQueueHandler::queue_rx_frame(
                    &mut core_ctx,
                    &mut bindings_ctx,
                    &FakeLinkDeviceId,
                    (),
                    body.clone(),
                ),
                Err(ReceiveQueueFullError(((), body)))
            );
            // We should only ever be woken up once when the first frame
            // was enqueued.
            assert_eq!(
                core::mem::take(&mut bindings_ctx.state_mut().woken_rx_tasks),
                [FakeLinkDeviceId]
            );

            assert!(MAX_RX_QUEUED_LEN > MAX_BATCH_SIZE);
            for i in (0..(MAX_RX_QUEUED_LEN - MAX_BATCH_SIZE)).step_by(MAX_BATCH_SIZE) {
                assert_eq!(
                    ReceiveQueueApi::handle_queued_rx_frames(
                        &mut core_ctx,
                        &mut bindings_ctx,
                        &FakeLinkDeviceId,
                    ),
                    WorkQueueReport::Pending
                );
                assert_eq!(
                    core::mem::take(&mut core_ctx.get_mut().handled_frames),
                    (i..i + MAX_BATCH_SIZE)
                        .map(|i| Buf::new(vec![i as u8], ..))
                        .collect::<Vec<_>>()
                );
            }

            assert_eq!(
                ReceiveQueueApi::handle_queued_rx_frames(
                    &mut core_ctx,
                    &mut bindings_ctx,
                    &FakeLinkDeviceId,
                ),
                WorkQueueReport::AllDone
            );
            assert_eq!(
                core::mem::take(&mut core_ctx.get_mut().handled_frames),
                (MAX_BATCH_SIZE * (MAX_RX_QUEUED_LEN / MAX_BATCH_SIZE - 1)..MAX_RX_QUEUED_LEN)
                    .map(|i| Buf::new(vec![i as u8], ..))
                    .collect::<Vec<_>>()
            );
            // Should not have woken up the RX task since the queue should be
            // empty.
            assert_eq!(core::mem::take(&mut bindings_ctx.state_mut().woken_rx_tasks), []);

            // The queue should now be empty so the next iteration of queueing
            // `MAX_RX_QUEUED_LEN` frames should succeed.
        }
    }
}
