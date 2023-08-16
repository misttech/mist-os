// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    diagnostics_log::{PublishError, Publisher, PublisherOptions},
    fidl_fuchsia_logger as flogger,
    thiserror::Error,
};

#[derive(Debug, Error)]
pub enum ScopedLoggerError {
    #[error("could not create publisher")]
    PublishError(#[source] PublishError),
}

pub struct ScopedLogger {
    publisher: Publisher,
}

impl ScopedLogger {
    pub fn create(logsink: flogger::LogSinkProxy) -> Result<Self, ScopedLoggerError> {
        let publisher = Publisher::new(
            PublisherOptions::default().wait_for_initial_interest(false).use_log_sink(logsink),
        )
        .map_err(ScopedLoggerError::PublishError)?;
        Ok(Self { publisher })
    }
}

impl tracing::Subscriber for ScopedLogger {
    #[inline]
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        self.publisher.enabled(metadata)
    }

    #[inline]
    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        self.publisher.new_span(span)
    }

    #[inline]
    fn record(&self, span: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        self.publisher.record(span, values)
    }

    #[inline]
    fn record_follows_from(&self, span: &tracing::span::Id, follows: &tracing::span::Id) {
        self.publisher.record_follows_from(span, follows)
    }

    #[inline]
    fn event(&self, event: &tracing::Event<'_>) {
        self.publisher.event(event)
    }

    #[inline]
    fn enter(&self, span: &tracing::span::Id) {
        self.publisher.enter(span)
    }

    #[inline]
    fn exit(&self, span: &tracing::span::Id) {
        self.publisher.exit(span)
    }
}
