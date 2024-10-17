// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#![deny(missing_docs)]

//! Provides the `tracing::Subscriber` implementation that allows to publish `tracing` events to
//! Fuchsia Logging System.
//! This library isn't Fuchsia-specific and provides a general `tracing::Subscriber` that allows
//! the library to also be used in the host.

use std::str::FromStr;
use thiserror::Error;

#[cfg(target_os = "fuchsia")]
mod fuchsia;
#[cfg(target_os = "fuchsia")]
use self::fuchsia as implementation;

#[cfg(not(target_os = "fuchsia"))]
mod portable;
#[cfg(not(target_os = "fuchsia"))]
use self::portable as implementation;

pub use fidl_fuchsia_diagnostics as fdiagnostics;
pub use implementation::*;

/// The severity of a log.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum Severity {
    /// Trace severity level
    Trace = 0x10,
    /// Debug severity level
    Debug = 0x20,
    /// Info severity level
    Info = 0x30,
    /// Warn severity level
    Warn = 0x40,
    /// Error severity level
    Error = 0x50,
    /// Fatal severity level
    Fatal = 0x60,
}

/// Returned when a string fails to parse into a severity.
#[derive(Error, Debug)]
#[error("The given severity isn't a valid severity")]
pub struct InvalidSeverity;

impl FromStr for Severity {
    type Err = InvalidSeverity;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "trace" => Ok(Severity::Trace),
            "debug" => Ok(Severity::Debug),
            "info" => Ok(Severity::Info),
            "warn" => Ok(Severity::Warn),
            "error" => Ok(Severity::Error),
            "fatal" => Ok(Severity::Fatal),
            _ => Err(InvalidSeverity),
        }
    }
}

impl From<Severity> for tracing::Level {
    fn from(s: Severity) -> tracing::Level {
        match s {
            Severity::Trace => tracing::Level::TRACE,
            Severity::Debug => tracing::Level::DEBUG,
            Severity::Info => tracing::Level::INFO,
            Severity::Warn => tracing::Level::WARN,
            Severity::Fatal | Severity::Error => tracing::Level::ERROR,
        }
    }
}

impl From<tracing::Level> for Severity {
    fn from(level: tracing::Level) -> Severity {
        match level {
            tracing::Level::TRACE => Severity::Trace,
            tracing::Level::DEBUG => Severity::Debug,
            tracing::Level::INFO => Severity::Info,
            tracing::Level::WARN => Severity::Warn,
            tracing::Level::ERROR => Severity::Error,
        }
    }
}

impl From<Severity> for fdiagnostics::Severity {
    fn from(s: Severity) -> fdiagnostics::Severity {
        match s {
            Severity::Trace => fdiagnostics::Severity::Trace,
            Severity::Debug => fdiagnostics::Severity::Debug,
            Severity::Info => fdiagnostics::Severity::Info,
            Severity::Warn => fdiagnostics::Severity::Warn,
            Severity::Error => fdiagnostics::Severity::Error,
            Severity::Fatal => fdiagnostics::Severity::Fatal,
        }
    }
}

impl From<fdiagnostics::Severity> for Severity {
    fn from(s: fdiagnostics::Severity) -> Severity {
        match s {
            fdiagnostics::Severity::Trace => Severity::Trace,
            fdiagnostics::Severity::Debug => Severity::Debug,
            fdiagnostics::Severity::Info => Severity::Info,
            fdiagnostics::Severity::Warn => Severity::Warn,
            fdiagnostics::Severity::Error => Severity::Error,
            fdiagnostics::Severity::Fatal => Severity::Fatal,
        }
    }
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
    pub(crate) install_panic_hook: bool,
    pub(crate) panic_prefix: Option<&'static str>,
}

impl<'t> Default for PublishOptions<'t> {
    fn default() -> Self {
        Self {
            publisher: PublisherOptions::default(),
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

    /// Enable to always log file/line information, otherwise only log
    /// when severity is ERROR or above.
    #[cfg(target_os = "fuchsia")]
    pub fn always_log_file_line(mut self) -> Self {
        self.publisher.always_log_file_line = true;
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
                pub fn minimum_severity(mut $self, severity: impl Into<Severity>) -> Self {
                    let this = &mut $self$(.$self_arg)*;
                    this.interest.min_severity = Some(severity.into().into());
                    $self
                }
            }
        )*
    };
}

publisher_options!((PublisherOptions, self,), (PublishOptions, self, publisher));
