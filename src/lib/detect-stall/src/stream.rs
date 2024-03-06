// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Support for running FIDL request streams until stalled.

use fidl::endpoints::RequestStream;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use futures::{
    channel::oneshot::{self, Receiver},
    ready, FutureExt, Stream, StreamExt,
};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use zx::Duration;

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
pub fn until_stalled<RS: RequestStream>(
    request_stream: RS,
    debounce_interval: Duration,
) -> (impl Stream<Item = <RS as Stream>::Item>, Receiver<Option<zx::Channel>>) {
    let (sender, receiver) = oneshot::channel();
    let stream = StallableRequestStream::new(request_stream, debounce_interval, move |channel| {
        let _ = sender.send(channel);
    });
    (stream, receiver)
}

/// The stream returned from [`until_stalled`].
pub struct StallableRequestStream<RS, F> {
    stream: Option<RS>,
    debounce_interval: Duration,
    unbind_callback: Option<F>,
    timer: Option<fasync::Timer>,
}

impl<RS, F> StallableRequestStream<RS, F> {
    /// Creates a new stallable request stream that will send the channel via `unbind_callback` when
    /// stream is stalled.
    pub fn new(stream: RS, debounce_interval: Duration, unbind_callback: F) -> Self {
        Self {
            stream: Some(stream),
            debounce_interval,
            unbind_callback: Some(unbind_callback),
            timer: None,
        }
    }
}

impl<RS: RequestStream + Unpin, F: FnOnce(Option<zx::Channel>) + Unpin> Stream
    for StallableRequestStream<RS, F>
{
    type Item = <RS as Stream>::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.stream.as_mut().expect("Stream already resolved").poll_next_unpin(cx) {
            Poll::Ready(message) => {
                self.timer = None;
                if message.is_none() {
                    self.unbind_callback.take().unwrap()(None);
                }
                Poll::Ready(message)
            }
            Poll::Pending => {
                let debounce_interval = self.debounce_interval;
                loop {
                    let timer =
                        self.timer.get_or_insert_with(|| fasync::Timer::new(debounce_interval));
                    ready!(timer.poll_unpin(cx));
                    self.timer = None;

                    // Try and unbind, which will fail if there are outstanding responders or
                    // control handles.
                    let (inner, is_terminated) = self.stream.take().unwrap().into_inner();
                    match Arc::try_unwrap(inner) {
                        Ok(inner) => {
                            self.unbind_callback.take().unwrap()(Some(
                                inner.into_channel().into_zx_channel(),
                            ));
                            return Poll::Ready(None);
                        }
                        Err(inner) => {
                            // We can't unbind because there are outstanding responders or control
                            // handles, so we'll try again after another debounce interval.
                            self.stream = Some(RS::from_inner(inner, is_terminated));
                        }
                    }
                }
            }
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

    #[fuchsia::test(allow_stalls = false)]
    async fn no_message() {
        let initial = fasync::Time::from_nanos(0);
        TestExecutor::advance_to(initial).await;
        const DURATION_NANOS: i64 = 1_000_000;
        let idle_duration = Duration::from_nanos(DURATION_NANOS);

        let (_proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>().unwrap();
        let (mut stream, stalled) = until_stalled(stream, idle_duration);

        assert_matches!(
            futures::join!(
                stream.next(),
                TestExecutor::advance_to(initial + idle_duration).then(|()| stalled)
            ),
            (None, Ok(Some(_)))
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn one_message() {
        let initial = fasync::Time::from_nanos(0);
        TestExecutor::advance_to(initial).await;
        const DURATION_NANOS: i64 = 1_000_000;
        let idle_duration = Duration::from_nanos(DURATION_NANOS);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>().unwrap();
        let (mut stream, stalled) = until_stalled(stream, idle_duration);

        pin_mut!(stalled);
        assert_matches!(TestExecutor::poll_until_stalled(&mut stalled).await, Poll::Pending);

        let _ = proxy.get_flags();

        let message = stream.next();
        pin_mut!(message);
        // Reply to the request so that the stream doesn't have any pending replies.
        let message = TestExecutor::poll_until_stalled(&mut message).await;
        let Poll::Ready(Some(Ok(fio::DirectoryRequest::GetFlags { responder }))) = message else {
            panic!("Unexpected {message:?}");
        };
        responder.send(zx::Status::OK.into_raw(), fio::OpenFlags::empty()).unwrap();

        // The stream hasn't stalled yet.
        TestExecutor::advance_to(initial + idle_duration * 2).await;
        assert!(TestExecutor::poll_until_stalled(&mut stalled).await.is_pending());

        // Poll the stream such that it is stalled.
        let message = stream.next();
        pin_mut!(message);
        assert_matches!(TestExecutor::poll_until_stalled(&mut message).await, Poll::Pending);
        assert_matches!(TestExecutor::poll_until_stalled(&mut stalled).await, Poll::Pending);

        TestExecutor::advance_to(initial + idle_duration * 3).await;

        // Now the the stream should be finished, because the channel has been unbound.
        assert_matches!(message.await, None);
        assert_matches!(stalled.await, Ok(Some(_)));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn pending_reply_blocks_stalling() {
        let initial = fasync::Time::from_nanos(0);
        TestExecutor::advance_to(initial).await;
        const DURATION_NANOS: i64 = 1_000_000;
        let idle_duration = Duration::from_nanos(DURATION_NANOS);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>().unwrap();
        let (stream, mut stalled) = until_stalled(stream, idle_duration);
        let mut stream = stream.fuse();

        let _ = proxy.get_flags();

        // Do not reply to the request, but hold on to the responder, so that there is a
        // pending reply in the connection.
        let message_with_pending_reply = stream.next().await.unwrap();
        let Ok(fio::DirectoryRequest::GetFlags { responder, .. }) = message_with_pending_reply
        else {
            panic!("Unexpected {message_with_pending_reply:?}");
        };

        // The connection does not stall, because there is a pending reply.
        TestExecutor::advance_to(initial + idle_duration * 2).await;
        futures::select! {
            _ = stream.next() => unreachable!(),
            _ = stalled => unreachable!(),
            default => {},
        }

        // Now we resolve the pending reply.
        responder.send(zx::Status::OK.into_raw(), fio::OpenFlags::empty()).unwrap();

        // The connection should stall.
        assert_matches!(
            futures::join!(
                stream.next(),
                TestExecutor::advance_to(initial + idle_duration * 3).then(|()| stalled)
            ),
            (None, Ok(Some(_)))
        );
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn completed_stream() {
        let initial = fasync::Time::from_nanos(0);
        TestExecutor::advance_to(initial).await;
        const DURATION_NANOS: i64 = 1_000_000;
        let idle_duration = Duration::from_nanos(DURATION_NANOS);

        let (proxy, stream) =
            fidl::endpoints::create_proxy_and_stream::<fio::DirectoryMarker>().unwrap();
        let (mut stream, stalled) = until_stalled(stream, idle_duration);

        pin_mut!(stalled);
        assert_matches!(TestExecutor::poll_until_stalled(&mut stalled).await, Poll::Pending);

        // Close the proxy such that the stream completes.
        drop(proxy);

        {
            // Read the `None` from the stream.
            assert_matches!(stream.next().await, None);

            // In practice the async tasks reading from the stream will exit, thus
            // dropping the stream. We'll emulate that here.
            drop(stream);
        }

        // Now the future should finish with `None` because the connection has
        // terminated without stalling.
        assert_matches!(stalled.await, Ok(None));
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

        // Ensure the server task can stop (by observing the completed stream).
        task.await;

        // Check that this channel was the original server endpoint.
        let client = proxy.into_channel().unwrap().into_zx_channel();
        assert_eq!(
            client.basic_info().unwrap().koid,
            (*server_end).as_ref().unwrap().as_ref().unwrap().basic_info().unwrap().related_koid
        );
    }
}
