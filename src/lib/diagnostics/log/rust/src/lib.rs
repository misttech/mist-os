// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#![deny(missing_docs)]

//! Provides the `tracing::Subscriber` implementation that allows to publish `tracing` events to
//! Fuchsia Logging System.
//! This library isn't Fuchsia-specific and provides a general `tracing::Subscriber` that allows
//! the library to also be used in the host.

use tracing::Level;
use tracing_log::LogTracer;

#[cfg(target_os = "fuchsia")]
mod fuchsia;
#[cfg(target_os = "fuchsia")]
use self::fuchsia as implementation;

#[cfg(not(target_os = "fuchsia"))]
mod portable;
#[cfg(not(target_os = "fuchsia"))]
use self::portable as implementation;

pub use fidl_fuchsia_diagnostics::{Interest, Severity};
pub use implementation::*;

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

/// Logs emitted with the `log` crate will be ingested by the registered `Publisher`.
pub(crate) fn ingest_log_events() -> Result<(), PublishError> {
    LogTracer::init()?;
    Ok(())
}

/// Adds a panic hook which will log an `ERROR` log with the panic information.
pub(crate) fn install_panic_hook(prefix: Option<&'static str>) {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let prefix = prefix.unwrap_or("PANIC");
        tracing::error!({ %info }, "{prefix}");
        previous_hook(info);
    }));
}

/// Options to configure publishing. This is for initialization of logs, it's a superset of
/// `PublisherOptions`.
pub struct PublishOptions<'t> {
    pub(crate) publisher: PublisherOptions<'t>,
    pub(crate) ingest_log_events: bool,
    pub(crate) install_panic_hook: bool,
    pub(crate) panic_prefix: Option<&'static str>,
}

impl<'t> Default for PublishOptions<'t> {
    fn default() -> Self {
        Self {
            publisher: PublisherOptions::default(),
            ingest_log_events: true,
            install_panic_hook: true,
            panic_prefix: None,
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

    /// Override the default string prefix for a logged panic message.
    pub fn panic_prefix(mut self, prefix: &'static str) -> Self {
        self.panic_prefix = Some(prefix);
        self
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
            }
        )*
    };
}

publisher_options!((PublisherOptions, self,), (PublishOptions, self, publisher));
