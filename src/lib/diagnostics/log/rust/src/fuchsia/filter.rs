// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

use diagnostics_log_encoding::{encode::TestRecord, Severity, SeverityExt};
use fidl::endpoints::Proxy;
use fidl_fuchsia_diagnostics::Interest;
use fidl_fuchsia_logger::{LogSinkProxy, LogSinkSynchronousProxy};
use fuchsia_zircon as zx;
use std::sync::{Mutex, RwLock};
use std::{future::Future, sync::Arc};
use tracing::{subscriber::Subscriber, Metadata};
use tracing_subscriber::layer::{Context, Layer};

use crate::OnInterestChanged;

pub(crate) struct InterestFilter {
    min_severity: Arc<RwLock<Severity>>,
    listener: Arc<Mutex<Option<Box<dyn OnInterestChanged + Send + Sync + 'static>>>>,
}

impl InterestFilter {
    /// Constructs a new `InterestFilter` and a future which should be polled to listen
    /// to changes in the LogSink's interest.
    pub fn new(
        proxy: LogSinkProxy,
        interest: Interest,
        wait_for_initial_interest: bool,
    ) -> (Self, impl Future<Output = ()>) {
        let default_severity = interest.min_severity.unwrap_or(Severity::Info);
        let (proxy, min_severity) = if wait_for_initial_interest {
            let sync_proxy = LogSinkSynchronousProxy::new(proxy.into_channel().unwrap().into());
            let initial_severity = match sync_proxy.wait_for_interest_change(zx::Time::INFINITE) {
                Ok(Ok(initial_interest)) => {
                    initial_interest.min_severity.unwrap_or(default_severity)
                }
                _ => default_severity,
            };
            (
                LogSinkProxy::new(
                    fidl::AsyncChannel::from_channel(sync_proxy.into_channel()).unwrap(),
                ),
                Arc::new(RwLock::new(initial_severity)),
            )
        } else {
            (proxy, Arc::new(RwLock::new(default_severity)))
        };

        let listener = Arc::new(Mutex::new(None));
        let filter = Self { min_severity: min_severity.clone(), listener: listener.clone() };
        (filter, Self::listen_to_interest_changes(listener, default_severity, min_severity, proxy))
    }

    /// Sets the minimum severity.
    pub fn set_minimum_severity(&self, severity: Severity) {
        let mut min_severity = self.min_severity.write().unwrap();
        *min_severity = severity;
    }

    /// Sets the interest listener.
    pub fn set_interest_listener<T>(&self, listener: T)
    where
        T: OnInterestChanged + Send + Sync + 'static,
    {
        let mut listener_guard = self.listener.lock().unwrap();
        *listener_guard = Some(Box::new(listener));
    }

    async fn listen_to_interest_changes(
        listener: Arc<Mutex<Option<Box<dyn OnInterestChanged + Send + Sync>>>>,
        default_severity: Severity,
        min_severity: Arc<RwLock<Severity>>,
        proxy: LogSinkProxy,
    ) {
        while let Ok(Ok(interest)) = proxy.wait_for_interest_change().await {
            let new_min_severity = {
                let mut min_severity_guard = min_severity.write().unwrap();
                *min_severity_guard = interest.min_severity.unwrap_or(default_severity);
                interest.min_severity.unwrap_or(default_severity)
            };
            let callback_guard = listener.lock().unwrap();
            if let Some(callback) = &*callback_guard {
                callback.on_changed(&new_min_severity);
            }
        }
    }

    pub fn enabled_for_testing(&self, record: &TestRecord<'_>) -> bool {
        let min_severity = self.min_severity.read().unwrap();
        record.severity >= *min_severity
    }
}

impl<S: Subscriber> Layer<S> for InterestFilter {
    /// Always returns `sometimes` so that we can later change the filter on the fly.
    fn register_callsite(
        &self,
        _metadata: &'static Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }

    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        let min_severity = self.min_severity.read().unwrap();
        metadata.severity() >= *min_severity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_logger::{LogSinkMarker, LogSinkRequest, LogSinkRequestStream};
    use futures::{channel::mpsc, StreamExt, TryStreamExt};
    use std::sync::Mutex;
    use tracing::{debug, error, info, trace, warn, Event};
    use tracing_subscriber::{layer::SubscriberExt, Registry};

    struct SeverityTracker {
        counts: Arc<Mutex<SeverityCount>>,
    }

    #[derive(Debug, Default, Eq, PartialEq)]
    struct SeverityCount {
        num_trace: u64,
        num_debug: u64,
        num_info: u64,
        num_warn: u64,
        num_error: u64,
    }

    impl<S: Subscriber> Layer<S> for SeverityTracker {
        fn on_event(&self, event: &Event<'_>, _cx: Context<'_, S>) {
            let mut count = self.counts.lock().unwrap();
            let to_increment = match event.metadata().severity() {
                Severity::Trace => &mut count.num_trace,
                Severity::Debug => &mut count.num_debug,
                Severity::Info => &mut count.num_info,
                Severity::Warn => &mut count.num_warn,
                Severity::Error => &mut count.num_error,
                Severity::Fatal => unreachable!("tracing crate doesn't have a fatal level"),
            };
            *to_increment += 1;
        }
    }

    struct InterestChangedListener(mpsc::UnboundedSender<()>);

    impl OnInterestChanged for InterestChangedListener {
        fn on_changed(&self, _: &Severity) {
            self.0.unbounded_send(()).unwrap();
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn default_filter_is_info_when_unspecified() {
        let (proxy, _requests) = create_proxy_and_stream::<LogSinkMarker>().unwrap();
        let (filter, _on_changes) = InterestFilter::new(proxy, Interest::default(), false);
        let observed = Arc::new(Mutex::new(SeverityCount::default()));
        tracing::subscriber::set_global_default(
            Registry::default().with(SeverityTracker { counts: observed.clone() }).with(filter),
        )
        .unwrap();
        let mut expected = SeverityCount::default();

        error!("oops");
        expected.num_error += 1;
        assert_eq!(&*observed.lock().unwrap(), &expected);

        warn!("maybe");
        expected.num_warn += 1;
        assert_eq!(&*observed.lock().unwrap(), &expected);

        info!("ok");
        expected.num_info += 1;
        assert_eq!(&*observed.lock().unwrap(), &expected);

        debug!("hint");
        assert_eq!(&*observed.lock().unwrap(), &expected, "should not increment counters");

        trace!("spew");
        assert_eq!(&*observed.lock().unwrap(), &expected, "should not increment counters");
    }

    async fn send_interest_change(stream: &mut LogSinkRequestStream, severity: Option<Severity>) {
        match stream.try_next().await {
            Ok(Some(LogSinkRequest::WaitForInterestChange { responder })) => {
                responder
                    .send(Ok(&Interest { min_severity: severity, ..Default::default() }))
                    .expect("send response");
            }
            other => panic!("Expected WaitForInterestChange but got {:?}", other),
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn default_filter_on_interest_changed() {
        let (proxy, mut requests) = create_proxy_and_stream::<LogSinkMarker>().unwrap();
        let (filter, on_changes) = InterestFilter::new(
            proxy,
            Interest { min_severity: Some(Severity::Warn), ..Default::default() },
            false,
        );
        let (send, mut recv) = mpsc::unbounded();
        filter.set_interest_listener(InterestChangedListener(send));
        let _on_changes_task = fuchsia_async::Task::spawn(on_changes);
        let observed = Arc::new(Mutex::new(SeverityCount::default()));
        tracing::subscriber::set_global_default(
            Registry::default().with(SeverityTracker { counts: observed.clone() }).with(filter),
        )
        .unwrap();

        // After overriding to info, filtering is at info level. The mpsc channel is used to
        // get a signal as to when the filter has processed the update.
        send_interest_change(&mut requests, Some(Severity::Info)).await;
        let _ = recv.next().await;

        let mut expected = SeverityCount::default();
        error!("oops");
        expected.num_error += 1;
        assert_eq!(&*observed.lock().unwrap(), &expected);

        warn!("maybe");
        expected.num_warn += 1;
        assert_eq!(&*observed.lock().unwrap(), &expected);

        info!("ok");
        expected.num_info += 1;
        assert_eq!(&*observed.lock().unwrap(), &expected);

        debug!("hint");
        assert_eq!(&*observed.lock().unwrap(), &expected, "should not increment counters");

        trace!("spew");
        assert_eq!(&*observed.lock().unwrap(), &expected, "should not increment counters");

        // After resetting to default, filtering is at warn level.
        send_interest_change(&mut requests, None).await;
        let _ = recv.next().await;

        error!("oops");
        expected.num_error += 1;
        assert_eq!(&*observed.lock().unwrap(), &expected);

        warn!("maybe");
        expected.num_warn += 1;
        assert_eq!(&*observed.lock().unwrap(), &expected);

        info!("ok");
        assert_eq!(&*observed.lock().unwrap(), &expected, "should not increment counters");

        debug!("hint");
        assert_eq!(&*observed.lock().unwrap(), &expected, "should not increment counters");

        trace!("spew");
        assert_eq!(&*observed.lock().unwrap(), &expected, "should not increment counters");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn wait_for_initial_interest() {
        let (proxy, mut requests) = create_proxy_and_stream::<LogSinkMarker>().unwrap();
        let t = std::thread::spawn(move || {
            // Unused, but its existence is needed by AsyncChannel.
            let _executor = fuchsia_async::LocalExecutor::new();
            let (filter, _on_changes) = InterestFilter::new(proxy, Interest::default(), true);
            filter
        });
        if let Some(Ok(request)) = requests.next().await {
            match request {
                LogSinkRequest::WaitForInterestChange { responder } => {
                    responder
                        .send(Ok(&Interest {
                            min_severity: Some(Severity::Trace),
                            ..Default::default()
                        }))
                        .expect("sent initial interest");
                }
                other => panic!("Got unexpected: {:?}", other),
            };
        }
        let filter = t.join().unwrap();
        assert_eq!(*filter.min_severity.read().unwrap(), Severity::Trace);
    }
}
