// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    diagnostics_log::{Publisher, PublisherOptions},
    fidl_fuchsia_io as fio, fidl_fuchsia_logger as flogger,
    fuchsia_component::client::connect_to_named_protocol_at_dir_root,
};

pub struct ScopedLogger {
    publisher: Publisher,
}

impl ScopedLogger {
    pub fn create(logsink: flogger::LogSinkProxy) -> Result<Self, Error> {
        let publisher = Publisher::new(
            PublisherOptions::default().wait_for_initial_interest(false).use_log_sink(logsink),
        )?;
        Ok(Self { publisher })
    }

    pub fn from_directory(dir: &fio::DirectoryProxy, path: &str) -> Result<Self, Error> {
        let sink = connect_to_named_protocol_at_dir_root::<flogger::LogSinkMarker>(dir, path)?;
        Self::create(sink)
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
