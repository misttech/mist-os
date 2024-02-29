// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Support for running FIDL request streams until stalled.

use fidl::endpoints::{RequestStream, ServerEnd};
use fuchsia_zircon as zx;
use futures::{stream::FusedStream, Future, FutureExt, Stream, StreamExt};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use zx::Duration;

use super::{Progress, StallDetector, Unbind};

/// [`until_stalled`] wraps a FIDL request stream of type [`RS`] into another
/// stream yielding the same requests, but could complete prematurely if it
/// has stalled, meaning:
///
/// - The underlying `request_stream` has no new messages.
/// - There are no pending FIDL replies.
/// - This condition has lasted for at least `debounce_interval`.
///
/// When that happens, the request stream will complete, and the returned future
/// will complete with the server endpoint which has been unbound from the
/// stream. The returned future will also complete if the request stream ended
/// on its own without stalling.
pub fn until_stalled<RS>(
    request_stream: RS,
    debounce_interval: Duration,
) -> (
    impl Stream<Item = <RS as Stream>::Item> + Unpin + FusedStream,
    impl Future<Output = Option<ServerEnd<<RS as RequestStream>::Protocol>>>,
)
where
    RS: Unpin + FusedStream + RequestStream + Stream,
{
    let (detector, progress) = StallDetector::new(debounce_interval);
    let stream = StallableRequestStream::new(request_stream, progress);
    (stream.fuse(), detector.wait().map(|option| option.map(Into::into)))
}

/// The stream returned from [`until_stalled`].
pub struct StallableRequestStream<RS> {
    inner: State<RS>,
    progress: Progress<NoPendingReply>,
}

impl<RS> StallableRequestStream<RS> {
    /// Creates a new stallable request stream that reports progress via `progress`.
    pub fn new(stream: RS, progress: Progress<NoPendingReply>) -> Self {
        Self { inner: State::Active(ActiveRequestStream(stream)), progress }
    }
}

enum State<RS> {
    Active(ActiveRequestStream<RS>),
    Stalled,
}

struct ActiveRequestStream<RS>(RS);

impl<RS> ActiveRequestStream<RS>
where
    RS: Unpin + FusedStream + RequestStream + Stream,
{
    fn try_into_inner(self) -> Result<NoPendingReply, Self> {
        // Note: we're relying on the fact that `RequestStream::into_inner` is
        // purely moving data, and does not cancel notifications to the executor.
        // This can be improved with better FIDL integrations.
        let (inner, is_terminated) = self.0.into_inner();
        match Arc::try_unwrap(inner) {
            Ok(inner) => Ok(NoPendingReply { inner, is_terminated }),
            Err(this) => return Err(Self(RS::from_inner(this, is_terminated))),
        }
    }

    fn from_inner(no_pending_reply: NoPendingReply) -> Self {
        Self(RS::from_inner(Arc::new(no_pending_reply.inner), no_pending_reply.is_terminated))
    }
}

/// A server endpoint that is unbound from a request stream and has no pending replies,
/// but nonetheless can wake up executors when the underlying channel receives a signal.
pub struct NoPendingReply {
    inner: fidl::ServeInner,
    is_terminated: bool,
}

impl Unbind for NoPendingReply {
    type Item = zx::Channel;

    fn unbind(self) -> zx::Channel {
        self.inner.into_channel().into_zx_channel()
    }
}

impl<RS> Stream for StallableRequestStream<RS>
where
    RS: Unpin + FusedStream + RequestStream + Stream,
{
    type Item = <RS as Stream>::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match std::mem::replace(&mut self.inner, State::Stalled) {
            State::Active(mut stream) => match stream.0.poll_next_unpin(cx) {
                Poll::Ready(message) => {
                    self.inner = State::Active(stream);
                    Poll::Ready(message)
                }
                Poll::Pending => {
                    match stream.try_into_inner() {
                        Err(stream) => {
                            // If there are outstanding responders or control handles, do not
                            // consider as stalled.
                            //
                            // Note: pathological case, if the server always processes the request
                            // concurrently as it reads more items from the stream, then we may
                            // never be able to unbind because there's always a pending responder.
                            // This should be rare if the server handles messages one at a time in
                            // a loop. If there's a need to unbind those kind of connections,
                            // we'll need to teach the FIDL bindings to wake us when the last
                            // pending responder is dropped. Note^2: the above case doesn't happen
                            // if one uses `for_each_concurrent` and responds inline, because the
                            // resulting future will always poll the stream whenever the resulting
                            // future is polled.
                            self.inner = State::Active(stream);
                        }
                        Ok(no_pending_reply) => {
                            self.inner = State::Stalled;
                            self.progress.stalled(no_pending_reply, cx);
                        }
                    }
                    Poll::Pending
                }
            },
            State::Stalled => match self.progress.resume() {
                Some(no_pending_reply) => {
                    self.inner = State::Active(ActiveRequestStream::from_inner(no_pending_reply));
                    self.poll_next(cx)
                }
                None => Poll::Ready(None),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fasync::TestExecutor;
    use fidl::endpoints::Proxy;
    use fidl::AsHandleRef;
    use fidl_fuchsia_io as fio;
    use fuchsia_async as fasync;
    use fuchsia_zircon as zx;
    use futures::{pin_mut, TryStreamExt};

    #[test]
    fn no_message() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        const START_NANOS: i64 = 1_000_000;
        const DURATION_NANOS: i64 = 1_000_000;
        let idle_duration = Duration::from_nanos(DURATION_NANOS);
        let () = exec.set_fake_time(fasync::Time::from_nanos(START_NANOS));

        let (_proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>().unwrap();
        let (mut stream, stalled) = until_stalled(stream, idle_duration);

        pin_mut!(stalled);
        assert!(exec.run_until_stalled(&mut stalled).is_pending());

        // Poll the stream such that it is stalled.
        let message = stream.next();
        pin_mut!(message);
        assert_matches!(exec.run_until_stalled(&mut message), Poll::Pending);

        // Now the detector should finish.
        assert!(exec.run_until_stalled(&mut stalled).is_pending());
        exec.set_fake_time(fasync::Time::after(idle_duration));
        assert_matches!(exec.run_until_stalled(&mut stalled), Poll::Ready(Some(_)));

        // Now the stream should be finished too, because the channel has been unbound.
        let message = stream.next();
        pin_mut!(message);
        assert_matches!(exec.run_until_stalled(&mut message), Poll::Ready(None));
    }

    #[test]
    fn one_message() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        const START_NANOS: i64 = 1_000_000;
        const DURATION_NANOS: i64 = 1_000_000;
        let idle_duration = Duration::from_nanos(DURATION_NANOS);
        let () = exec.set_fake_time(fasync::Time::from_nanos(START_NANOS));

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>().unwrap();
        let (mut stream, stalled) = until_stalled(stream, idle_duration);

        pin_mut!(stalled);
        assert!(exec.run_until_stalled(&mut stalled).is_pending());

        let _ = proxy.get_flags();

        let message = stream.next();
        pin_mut!(message);
        // Reply to the request so that the stream doesn't have any pending replies.
        let message = exec.run_until_stalled(&mut message);
        let Poll::Ready(Some(Ok(fio::DirectoryRequest::GetFlags { responder }))) = message else {
            panic!("Unexpected {message:?}");
        };
        responder.send(zx::Status::OK.into_raw(), fio::OpenFlags::empty()).unwrap();

        // The stream hasn't stalled yet.
        exec.set_fake_time(fasync::Time::after(idle_duration * 2));
        assert!(exec.run_until_stalled(&mut stalled).is_pending());

        // Poll the stream such that it is stalled.
        let message = stream.next();
        pin_mut!(message);
        assert_matches!(exec.run_until_stalled(&mut message), Poll::Pending);

        // Now the detector should finish.
        assert!(exec.run_until_stalled(&mut stalled).is_pending());
        exec.set_fake_time(fasync::Time::after(idle_duration));
        assert_matches!(exec.run_until_stalled(&mut stalled), Poll::Ready(Some(_)));

        // Now the stream should be finished too, because the channel has been unbound.
        let message = stream.next();
        pin_mut!(message);
        assert_matches!(exec.run_until_stalled(&mut message), Poll::Ready(None));
    }

    #[test]
    fn pending_reply_blocks_stalling() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        const START_NANOS: i64 = 1_000_000;
        const DURATION_NANOS: i64 = 1_000_000;
        let idle_duration = Duration::from_nanos(DURATION_NANOS);
        let () = exec.set_fake_time(fasync::Time::from_nanos(START_NANOS));

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>().unwrap();
        let (mut stream, stalled) = until_stalled(stream, idle_duration);

        pin_mut!(stalled);
        assert!(exec.run_until_stalled(&mut stalled).is_pending());

        let _ = proxy.get_flags();

        let message = stream.next();
        pin_mut!(message);
        // Do not reply to the request, but hold on to the message, so that there is a
        // pending reply in the connection.
        let message_with_pending_reply = exec.run_until_stalled(&mut message);
        assert_matches!(
            &message_with_pending_reply,
            Poll::Ready(Some(Ok(fio::DirectoryRequest::GetFlags { .. })))
        );

        // The detector hasn't finished yet.
        exec.set_fake_time(fasync::Time::after(idle_duration * 2));
        assert!(exec.run_until_stalled(&mut stalled).is_pending());

        // Poll the stream such that it is pending.
        let message = stream.next();
        pin_mut!(message);
        assert_matches!(exec.run_until_stalled(&mut message), Poll::Pending);

        // Now the detector should still not finish, because there is a pending
        // reply to `GetFlags`.
        exec.set_fake_time(fasync::Time::after(idle_duration));
        assert!(exec.run_until_stalled(&mut stalled).is_pending());

        // Now we resolve the pending reply.
        let Poll::Ready(Some(Ok(fio::DirectoryRequest::GetFlags { responder, .. }))) =
            message_with_pending_reply
        else {
            panic!("Unexpected {message_with_pending_reply:?}");
        };
        responder.send(zx::Status::OK.into_raw(), fio::OpenFlags::empty()).unwrap();

        // Run the stream and detector. The detector should now finish.
        let message = stream.next();
        pin_mut!(message);
        assert_matches!(exec.run_until_stalled(&mut message), Poll::Pending);

        assert!(exec.run_until_stalled(&mut stalled).is_pending());
        exec.set_fake_time(fasync::Time::after(idle_duration));
        assert_matches!(exec.run_until_stalled(&mut stalled), Poll::Ready(Some(_)));
    }

    #[test]
    fn completed_stream() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        const START_NANOS: i64 = 1_000_000;
        const DURATION_NANOS: i64 = 1_000_000;
        let idle_duration = Duration::from_nanos(DURATION_NANOS);
        let () = exec.set_fake_time(fasync::Time::from_nanos(START_NANOS));

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>().unwrap();
        let (mut stream, stalled) = until_stalled(stream, idle_duration);

        pin_mut!(stalled);
        assert!(exec.run_until_stalled(&mut stalled).is_pending());

        // Close the proxy such that the stream completes.
        drop(proxy);

        {
            // Read the `None` from the stream.
            let message = stream.next();
            pin_mut!(message);
            assert_matches!(exec.run_until_stalled(&mut message), Poll::Ready(None));

            // In practice the async tasks reading from the stream will exit, thus
            // dropping the stream. We'll emulate that here.
            drop(message);
            drop(stream);
        }

        // Now the future should finish with `None` because the connection has
        // terminated without stalling.
        assert!(exec.run_until_stalled(&mut stalled).is_pending());
        exec.set_fake_time(fasync::Time::after(idle_duration));
        assert_matches!(exec.run_until_stalled(&mut stalled), Poll::Ready(None));
    }

    /// Simulate what would happen when a component serves a FIDL stream that's been
    /// wrapped in `until_stalled`, and thus will complete and give the unbound channel
    /// back to the user, who can then pass it back to `component_manager` in practice.
    #[fuchsia::test(allow_stalls = false)]
    async fn end_to_end() {
        let initial = fasync::Time::from_nanos(0);
        TestExecutor::advance_to(initial).await;
        use fidl_fuchsia_component_client_test::{ServiceAMarker, ServiceARequest};

        const DURATION_NANOS: i64 = 40_000_000;
        let idle_duration = Duration::from_nanos(DURATION_NANOS);
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<ServiceAMarker>().unwrap();
        let (mut stream, stalled) = until_stalled(stream, idle_duration);

        // Launch a task that serves the stream.
        let task = fasync::Task::spawn(async move {
            while let Some(request) = stream.try_next().await.unwrap() {
                match request {
                    ServiceARequest::Foo { responder } => responder.send().unwrap(),
                }
            }
        });

        // Launch another task to await the `stalled` future and also let us
        // check for it synchronously.
        let stalled = fasync::Task::spawn(stalled).map(Arc::new).shared();

        // Make some requests at intervals half the idle duration. Stall should not happen.
        let request_duration = Duration::from_nanos(DURATION_NANOS / 2);
        const NUM_REQUESTS: usize = 5;
        let mut deadline = initial;
        for _ in 0..NUM_REQUESTS {
            proxy.foo().await.unwrap();
            deadline += request_duration;
            TestExecutor::advance_to(deadline).await;
            assert!(stalled.clone().now_or_never().is_none());
        }

        // Wait for stalling.
        deadline += idle_duration;
        TestExecutor::advance_to(deadline).await;
        let server_end = stalled.await;
        assert!(server_end.is_some());

        // Ensure the server task can stop (by observing the completed stream).
        task.await;

        // Check that this channel was the original server endpoint.
        let client = proxy.into_channel().unwrap().into_zx_channel();
        assert_eq!(
            client.basic_info().unwrap().koid,
            (*server_end).as_ref().unwrap().basic_info().unwrap().related_koid
        );
    }
}
