// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#![deny(missing_docs)]

//! Publish diagnostics as a stream of log messages.

use fidl_fuchsia_diagnostics_stream::Record;
use fidl_fuchsia_logger::{LogSinkMarker, LogSinkProxy};
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_zircon::{self as zx};
use std::{any::TypeId, collections::HashSet, fmt::Debug};
use thiserror::Error;
use tracing::{
    span::{Attributes, Id, Record as TracingRecord},
    subscriber::Subscriber,
    Event, Level, Metadata,
};
use tracing_core::span::Current;
use tracing_log::LogTracer;
use tracing_subscriber::{layer::Layered, prelude::*, registry::Registry};
mod filter;
mod sink;

use filter::InterestFilter;
use sink::Sink;

// Publicly export `Interest` and `Severity` since they are part of the `PublishOptions` API.
pub use diagnostics_log_encoding::Metatag;
pub use fidl_fuchsia_diagnostics::{Interest, Severity};

/// Callback for interest listeners
pub trait OnInterestChanged {
    /// Callback for when the interest changes
    fn on_changed(&self, severity: &Severity);
}

/// Options to configure publishing. This is for initialization of logs, it's a superset of
/// `PublisherOptions`.
pub struct PublishOptions<'t> {
    publisher: PublisherOptions<'t>,
    ingest_log_events: bool,
    install_panic_hook: bool,
}

impl<'t> Default for PublishOptions<'t> {
    fn default() -> Self {
        Self {
            publisher: PublisherOptions::default(),
            ingest_log_events: true,
            install_panic_hook: true,
        }
    }
}

impl<'t> PublishOptions<'t> {
    /// Whether or not to install a panic hook which will log an ERROR whenever a panic happens.
    ///
    /// Default: true.
    pub fn install_panic_hook(mut self, enable: bool) -> Self {
        self.install_panic_hook = enable;
        self
    }

    /// Whether or not to ingest log events emitted by the `log` crate macros.
    ///
    /// Default: true.
    pub fn ingest_log_events(mut self, enable: bool) -> Self {
        self.ingest_log_events = enable;
        self
    }
}

/// Options to configure a `Publisher`.
pub struct PublisherOptions<'t> {
    interest: Interest,
    listen_for_interest_updates: bool,
    log_sink_proxy: Option<LogSinkProxy>,
    metatags: HashSet<Metatag>,
    tags: &'t [&'t str],
    wait_for_initial_interest: bool,
}

impl<'t> Default for PublisherOptions<'t> {
    fn default() -> Self {
        Self {
            interest: Interest::EMPTY,
            listen_for_interest_updates: true,
            log_sink_proxy: None,
            metatags: HashSet::new(),
            tags: &[],
            wait_for_initial_interest: true,
        }
    }
}

impl PublisherOptions<'_> {
    /// Creates a `PublishOptions` with all sets either empty or set to false. This is
    /// useful when fine grain control of `Publisher` and its behavior is necessary.
    ///
    /// However, for the majority of binaries that "just want to log",
    /// `PublishOptions::default` is preferred as that brings all the default
    /// configuration that is desired in most scenarios.
    pub fn empty() -> Self {
        Self {
            interest: Interest::EMPTY,
            listen_for_interest_updates: false,
            log_sink_proxy: None,
            metatags: HashSet::new(),
            tags: &[],
            wait_for_initial_interest: false,
        }
    }
}

macro_rules! publisher_options {
    ($(($name:ident, $self:ident, $($self_arg:ident),*)),*) => {
        $(
            impl<'t> $name<'t> {
                /// Sets the tags applied to all published events.
                ///
                /// When set to an empty slice (the default), events are tagged with the moniker of
                /// the component in which they are recorded.
                ///
                /// Default: empty.
                pub fn tags(mut $self, tags: &'t [&'t str]) -> Self {
                    let this = &mut $self$(.$self_arg)*;
                    this.tags = tags;
                    $self
                }

                /// Enable a metatag. It'll be applied to all published events.
                ///
                /// Default: no metatags are enabled.
                pub fn enable_metatag(mut $self, metatag: Metatag) -> Self {
                    let this = &mut $self$(.$self_arg)*;
                    this.metatags.insert(metatag);
                    $self
                }

                /// An interest filter to apply to messages published.
                ///
                /// Default: EMPTY, which implies INFO.
                pub fn minimum_severity(mut $self, severity: impl IntoSeverity) -> Self {
                    let this = &mut $self$(.$self_arg)*;
                    this.interest.min_severity = Some(severity.into_severity());
                    $self
                }

                /// Whether or not to block on initial runtime interest being received before
                /// starting to emit log records using the default interest configured.
                ///
                /// It's recommended that this is set when
                /// developing to guarantee that a dynamically configured minimum severity makes it
                /// to the component before it starts emitting logs.
                ///
                /// Default: true.
                pub fn wait_for_initial_interest(mut $self, enable: bool) -> Self {
                    let this = &mut $self$(.$self_arg)*;
                    this.wait_for_initial_interest = enable;
                    $self
                }

                /// When set, a `fuchsia_async::Task` will be spawned and held that will be
                /// listening for interest changes.
                ///
                /// Default: true
                pub fn listen_for_interest_updates(mut $self, enable: bool) -> Self {
                    let this = &mut $self$(.$self_arg)*;
                    this.listen_for_interest_updates = enable;
                    $self
                }

                /// Sets the `LogSink` that will be used.
                ///
                /// Default: the `fuchsia.logger.LogSink` available in the incoming namespace.
                pub fn use_log_sink(mut $self, proxy: LogSinkProxy) -> Self {
                    let this = &mut $self$(.$self_arg)*;
                    this.log_sink_proxy = Some(proxy);
                    $self
                }
            }
        )*
    };
}

publisher_options!((PublisherOptions, self,), (PublishOptions, self, publisher));

/// Logs emitted with the `log` crate will be ingested by the registered `Publisher`.
pub fn ingest_log_events() -> Result<(), PublishError> {
    LogTracer::init()?;
    Ok(())
}

/// Adds a panic hook which will log an `ERROR` log with the panic information.
pub fn install_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!({ %info }, "PANIC");
        previous_hook(info);
    }));
}

/// Initializes logging with the given options.
///
/// IMPORTANT: this should be called at most once in a program, otherwise it'll return errors or
/// panic. Therefore it's recommended to never call this from libraries and only do it from
/// binaries.
pub fn initialize(opts: PublishOptions<'_>) -> Result<(), PublishError> {
    let publisher = Publisher::new(opts.publisher)?;

    if opts.ingest_log_events {
        ingest_log_events()?;
    }

    if opts.install_panic_hook {
        install_panic_hook();
    }

    tracing::subscriber::set_global_default(publisher)?;
    Ok(())
}

/// Trait that allows to convert a type into a `fidl_fuchsia_diagnostics::Severity`
pub trait IntoSeverity {
    /// Returns a Severity.
    fn into_severity(self) -> Severity;
}

impl IntoSeverity for Severity {
    fn into_severity(self) -> Severity {
        self
    }
}

impl IntoSeverity for Level {
    fn into_severity(self) -> Severity {
        match self {
            Level::TRACE => Severity::Trace,
            Level::DEBUG => Severity::Debug,
            Level::INFO => Severity::Info,
            Level::WARN => Severity::Warn,
            Level::ERROR => Severity::Error,
        }
    }
}

/// A `Publisher` acts as broker, implementing [`tracing::Subscriber`] to receive diagnostic
/// events from a component, and then forwarding that data on to a diagnostics service.
pub struct Publisher {
    inner: Layered<InterestFilter, Layered<Sink, Registry>>,
    interest_listening_task: Option<fasync::Task<()>>,
}

impl Default for Publisher {
    fn default() -> Self {
        Self::new(PublisherOptions::default()).expect("failed to create Publisher")
    }
}

impl Publisher {
    /// Construct a new `Publisher` using the given options.
    ///
    /// If options such as `install_panic_hook` and `ingest_log_events` are enabled, then this
    /// constructor should be called only once.
    pub fn new(opts: PublisherOptions<'_>) -> Result<Self, PublishError> {
        let proxy = match opts.log_sink_proxy {
            Some(log_sink) => log_sink,
            None => connect_to_protocol::<LogSinkMarker>()
                .map_err(|e| e.to_string())
                .map_err(PublishError::LogSinkConnect)?,
        };
        let sink = Sink::new(&proxy, opts.tags, opts.metatags)?;
        let (filter, on_change) =
            InterestFilter::new(proxy, opts.interest, opts.wait_for_initial_interest);
        let interest_listening_task = if opts.listen_for_interest_updates {
            Some(fasync::Task::spawn(on_change))
        } else {
            None
        };

        Ok(Self { inner: Registry::default().with(sink).with(filter), interest_listening_task })
    }

    // TODO(fxbug.dev/71242) delete this and make Publisher private
    /// Publish the provided event for testing.
    pub fn event_for_testing(&self, file: &str, line: u32, record: Record) {
        let filter: &InterestFilter = (&self.inner as &dyn Subscriber).downcast_ref().unwrap();
        if filter.enabled_for_testing(file, line, &record) {
            let sink: &Sink = (&self.inner as &dyn Subscriber).downcast_ref().unwrap();
            sink.event_for_testing(file, line, record);
        }
    }

    /// Registers an interest listener
    pub fn set_interest_listener<T>(&self, listener: T)
    where
        T: OnInterestChanged + Send + Sync + 'static,
    {
        let filter: &InterestFilter = (&self.inner as &dyn Subscriber).downcast_ref().unwrap();
        filter.set_interest_listener(listener);
    }

    /// Takes the task listening for interest changes if one exists.
    pub fn take_interest_listening_task(&mut self) -> Option<fasync::Task<()>> {
        self.interest_listening_task.take()
    }
}

impl Subscriber for Publisher {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.inner.enabled(metadata)
    }
    fn new_span(&self, span: &Attributes<'_>) -> Id {
        self.inner.new_span(span)
    }
    fn record(&self, span: &Id, values: &TracingRecord<'_>) {
        self.inner.record(span, values)
    }
    fn record_follows_from(&self, span: &Id, follows: &Id) {
        self.inner.record_follows_from(span, follows)
    }
    fn event(&self, event: &Event<'_>) {
        self.inner.event(event)
    }
    fn enter(&self, span: &Id) {
        self.inner.enter(span)
    }
    fn exit(&self, span: &Id) {
        self.inner.exit(span)
    }
    fn register_callsite(
        &self,
        metadata: &'static Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        self.inner.register_callsite(metadata)
    }
    fn clone_span(&self, id: &Id) -> Id {
        self.inner.clone_span(id)
    }
    fn try_close(&self, id: Id) -> bool {
        self.inner.try_close(id)
    }
    fn current_span(&self) -> Current {
        self.inner.current_span()
    }
    unsafe fn downcast_raw(&self, id: TypeId) -> Option<*const ()> {
        if id == TypeId::of::<Self>() {
            Some(self as *const Self as *const ())
        } else {
            None
        }
    }
}

/// Errors arising while forwarding a diagnostics stream to the environment.
#[derive(Debug, Error)]
pub enum PublishError {
    /// Connection to fuchsia.logger.LogSink failed.
    #[error("failed to connect to fuchsia.logger.LogSink ({0})")]
    LogSinkConnect(String),

    /// Couldn't create a new socket.
    #[error("failed to create a socket for logging")]
    MakeSocket(#[source] zx::Status),

    /// An issue with the LogSink channel or socket prevented us from sending it to the `LogSink`.
    #[error("failed to send a socket to the LogSink")]
    SendSocket(#[source] fidl::Error),

    /// Setting the default global [`tracing::Subscriber`] failed.
    #[error("failed to install forwarder as the global default")]
    SetGlobalDefault(#[from] tracing::subscriber::SetGlobalDefaultError),

    /// Installing a forwarder from [`log`] macros to [`tracing`] macros failed.
    #[error("failed to install a forwarder from `log` to `tracing`")]
    InitLogForward(#[from] tracing_log::log_tracer::SetLoggerError),
}
