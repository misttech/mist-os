// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::Poll;
use std::{fmt, mem};

use crate::runtime::{EHandle, PacketReceiver, ReceiverRegistration};
use futures::task::{AtomicWaker, Context};
use zx::{self as zx, AsHandleRef};

struct OnSignalsReceiver {
    maybe_signals: AtomicUsize,
    task: AtomicWaker,
}

impl OnSignalsReceiver {
    fn get_signals(&self, cx: &mut Context<'_>) -> Poll<zx::Signals> {
        let mut signals = self.maybe_signals.load(Ordering::Relaxed);
        if signals == 0 {
            // No signals were received-- register to receive a wakeup when they arrive.
            self.task.register(cx.waker());
            // Check again for signals after registering for a wakeup in case signals
            // arrived between registering and the initial load of signals
            signals = self.maybe_signals.load(Ordering::SeqCst);
        }
        if signals == 0 {
            Poll::Pending
        } else {
            Poll::Ready(zx::Signals::from_bits_truncate(signals as u32))
        }
    }

    fn set_signals(&self, signals: zx::Signals) {
        self.maybe_signals.store(signals.bits() as usize, Ordering::SeqCst);
        self.task.wake();
    }
}

impl PacketReceiver for OnSignalsReceiver {
    fn receive_packet(&self, packet: zx::Packet) {
        let observed = if let zx::PacketContents::SignalOne(p) = packet.contents() {
            p.observed()
        } else {
            return;
        };

        self.set_signals(observed);
    }
}

/// A future that completes when some set of signals become available on a Handle.
#[must_use = "futures do nothing unless polled"]
pub struct OnSignals<'a, H: AsHandleRef> {
    handle: H,
    signals: zx::Signals,
    registration: Option<ReceiverRegistration<OnSignalsReceiver>>,
    phantom: PhantomData<&'a H>,
}

/// Alias for the common case where OnSignals is used with zx::HandleRef.
pub type OnSignalsRef<'a> = OnSignals<'a, zx::HandleRef<'a>>;

impl<'a, H: AsHandleRef + 'a> OnSignals<'a, H> {
    /// Creates a new `OnSignals` object which will receive notifications when
    /// any signals in `signals` occur on `handle`.
    pub fn new(handle: H, signals: zx::Signals) -> Self {
        // We don't register for the signals until first polled.  When we are first polled, we'll
        // check to see if the signals are set and if they are, we're done.  If they aren't, we then
        // register for an asynchronous notification via the port.
        //
        // We could change the code to register for the asynchronous notification here, but then
        // when first polled, if the notification hasn't arrived, we'll still check to see if the
        // signals are set (see below for the reason why).  Given that the time between construction
        // and when we first poll is typically small, registering here probably won't make much
        // difference (and on a single-threaded executor, a notification is unlikely to be processed
        // before the first poll anyway).  The way we have it now means we don't have to register at
        // all if the signals are already set, which will be a win some of the time.
        OnSignals { handle, signals, registration: None, phantom: PhantomData }
    }

    /// Takes the handle.
    pub fn take_handle(mut self) -> H
    where
        H: zx::HandleBased,
    {
        self.unregister();
        std::mem::replace(&mut self.handle, zx::Handle::invalid().into())
    }

    /// This function allows the `OnSignals` object to live for the `'static` lifetime, at the cost
    /// of disabling automatic cleanup of the port wait.
    ///
    /// WARNING: Do not use unless you can guarantee that either:
    /// - The future is not dropped before it completes, or
    /// - The handle is dropped without creating additional OnSignals futures for it.
    ///
    /// Creating an OnSignals calls zx_object_wait_async, which consumes a small amount of kernel
    /// resources. Dropping the OnSignals calls zx_port_cancel to clean up. But calling
    /// extend_lifetime disables this cleanup, since the zx_port_wait call requires a reference to
    /// the handle. The port registration can also be cleaned up by closing the handle or by
    /// waiting for the signal to be triggered. But if neither of these happens, the registration
    /// is leaked. This wastes kernel memory and the kernel will eventually kill your process to
    /// force a cleanup.
    ///
    /// Note that `OnSignals` will not fire if the handle that was used to create it is dropped or
    /// transferred to another process.
    // TODO(https://fxbug.dev/42182035): Try to remove this footgun.
    pub fn extend_lifetime(mut self) -> LeakedOnSignals {
        match self.registration.take() {
            Some(r) => LeakedOnSignals { registration: Ok(r) },
            None => LeakedOnSignals { registration: self.register(None) },
        }
    }

    fn register(
        &self,
        cx: Option<&mut Context<'_>>,
    ) -> Result<ReceiverRegistration<OnSignalsReceiver>, zx::Status> {
        let registration = EHandle::local().register_receiver(Arc::new(OnSignalsReceiver {
            maybe_signals: AtomicUsize::new(0),
            task: AtomicWaker::new(),
        }));

        // If a context has been supplied, we must register it now before calling
        // `wait_async_handle` below to avoid races.
        if let Some(cx) = cx {
            registration.task.register(cx.waker());
        }

        self.handle.wait_async_handle(
            registration.port(),
            registration.key(),
            self.signals,
            zx::WaitAsyncOpts::empty(),
        )?;

        Ok(registration)
    }

    fn unregister(&mut self) {
        if let Some(registration) = self.registration.take() {
            if registration.receiver().maybe_signals.load(Ordering::SeqCst) == 0 {
                // Ignore the error from zx_port_cancel, because it might just be a race condition.
                // If the packet is handled between the above maybe_signals check and the port
                // cancel, it will fail with ZX_ERR_NOT_FOUND, and we can't do anything about it.
                let _ = registration.port().cancel(&self.handle, registration.key());
            }
        }
    }
}

impl<H: AsHandleRef + Unpin> Future for OnSignals<'_, H> {
    type Output = Result<zx::Signals, zx::Status>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &self.registration {
            None => {
                match self
                    .handle
                    .wait_handle(self.signals, zx::MonotonicInstant::INFINITE_PAST)
                    .to_result()
                {
                    Ok(signals) => Poll::Ready(Ok(signals)),
                    Err(zx::Status::TIMED_OUT) => {
                        let registration = self.register(Some(cx))?;
                        self.get_mut().registration = Some(registration);
                        Poll::Pending
                    }
                    Err(e) => Poll::Ready(Err(e)),
                }
            }
            Some(r) => match r.receiver().get_signals(cx) {
                Poll::Ready(signals) => Poll::Ready(Ok(signals)),
                Poll::Pending => {
                    // We haven't received a notification for the signals, but we still want to poll
                    // the kernel in case the notification hasn't been processed yet by the
                    // executor.  This behaviour is relied upon in some cases: in Component Manager,
                    // in some shutdown paths, it wants to drain and process all messages in
                    // channels before it closes them.  There is no other reliable way to flush a
                    // pending notification (particularly on a multi-threaded executor).  This will
                    // incur a small performance penalty in the case that this future has been
                    // polled when no notification was actually received (such as can be the case
                    // with some futures combinators).
                    match self
                        .handle
                        .wait_handle(self.signals, zx::MonotonicInstant::INFINITE_PAST)
                        .to_result()
                    {
                        Ok(signals) => Poll::Ready(Ok(signals)),
                        Err(_) => Poll::Pending,
                    }
                }
            },
        }
    }
}

impl<H: AsHandleRef> fmt::Debug for OnSignals<'_, H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OnSignals")
    }
}

impl<H: AsHandleRef> Drop for OnSignals<'_, H> {
    fn drop(&mut self) {
        self.unregister();
    }
}

impl<H: AsHandleRef> AsHandleRef for OnSignals<'_, H> {
    fn as_handle_ref(&self) -> zx::HandleRef<'_> {
        self.handle.as_handle_ref()
    }
}

impl<H: AsHandleRef> AsRef<H> for OnSignals<'_, H> {
    fn as_ref(&self) -> &H {
        &self.handle
    }
}

pub struct LeakedOnSignals {
    registration: Result<ReceiverRegistration<OnSignalsReceiver>, zx::Status>,
}

impl Future for LeakedOnSignals {
    type Output = Result<zx::Signals, zx::Status>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let reg = self.registration.as_mut().map_err(|e| mem::replace(e, zx::Status::OK))?;
        reg.receiver().get_signals(cx).map(Ok)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::TestExecutor;
    use assert_matches::assert_matches;
    use futures::future::{pending, FutureExt};
    use futures::task::{waker, ArcWake};
    use std::pin::pin;

    #[test]
    fn wait_for_event() -> Result<(), zx::Status> {
        let mut exec = crate::TestExecutor::new();
        let mut deliver_events =
            || assert!(exec.run_until_stalled(&mut pending::<()>()).is_pending());

        let event = zx::Event::create();
        let mut signals = OnSignals::new(&event, zx::Signals::EVENT_SIGNALED);
        let (waker, waker_count) = futures_test::task::new_count_waker();
        let cx = &mut std::task::Context::from_waker(&waker);

        // Check that `signals` is still pending before the event has been signaled
        assert_eq!(signals.poll_unpin(cx), Poll::Pending);
        deliver_events();
        assert_eq!(waker_count, 0);
        assert_eq!(signals.poll_unpin(cx), Poll::Pending);

        // signal the event and check that `signals` has been woken up and is
        // no longer pending
        event.signal_handle(zx::Signals::NONE, zx::Signals::EVENT_SIGNALED)?;
        deliver_events();
        assert_eq!(waker_count, 1);
        assert_eq!(signals.poll_unpin(cx), Poll::Ready(Ok(zx::Signals::EVENT_SIGNALED)));

        Ok(())
    }

    #[test]
    fn drop_before_event() {
        let mut fut = std::pin::pin!(async {
            let ehandle = EHandle::local();

            let event = zx::Event::create();
            let mut signals = OnSignals::new(&event, zx::Signals::EVENT_SIGNALED);
            assert_eq!(futures::poll!(&mut signals), Poll::Pending);
            let key = signals.registration.as_ref().unwrap().key();

            std::mem::drop(signals);
            assert!(ehandle.port().cancel(&event, key) == Err(zx::Status::NOT_FOUND));

            // try again but with extend_lifetime
            let signals = OnSignals::new(&event, zx::Signals::EVENT_SIGNALED).extend_lifetime();
            let key = signals.registration.as_ref().unwrap().key();
            std::mem::drop(signals);
            assert!(ehandle.port().cancel(&event, key) == Ok(()));
        });

        assert!(TestExecutor::new().run_until_stalled(&mut fut).is_ready());
    }

    #[test]
    fn test_always_polls() {
        let mut exec = TestExecutor::new();

        let (rx, tx) = zx::Channel::create();

        let mut fut = pin!(OnSignals::new(&rx, zx::Signals::CHANNEL_READABLE));

        assert_eq!(exec.run_until_stalled(&mut fut), Poll::Pending);

        tx.write(b"hello", &mut []).expect("write failed");

        struct Waker;
        impl ArcWake for Waker {
            fn wake_by_ref(_arc_self: &Arc<Self>) {}
        }

        // Poll the future directly which guarantees the port notification for the write hasn't
        // arrived.
        assert_matches!(
            fut.poll(&mut Context::from_waker(&waker(Arc::new(Waker)))),
            Poll::Ready(Ok(signals)) if signals.contains(zx::Signals::CHANNEL_READABLE)
        );
    }

    #[test]
    fn test_take_handle() {
        let mut exec = TestExecutor::new();

        let (rx, tx) = zx::Channel::create();

        let mut fut = OnSignals::new(rx, zx::Signals::CHANNEL_READABLE);

        assert_eq!(exec.run_until_stalled(&mut fut), Poll::Pending);

        tx.write(b"hello", &mut []).expect("write failed");

        assert_matches!(exec.run_until_stalled(&mut fut), Poll::Ready(Ok(_)));

        let mut message = zx::MessageBuf::new();
        fut.take_handle().read(&mut message).unwrap();

        assert_eq!(message.bytes(), b"hello");
    }
}
