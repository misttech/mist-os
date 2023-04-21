// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    diagnostics_log::{Publisher, PublisherOptions},
    fidl_fuchsia_io as fio,
    fidl_fuchsia_logger::LogSinkMarker,
    fuchsia_component::client::connect_to_named_protocol_at_dir_root,
};

pub struct ScopedLogger {
    publisher: Publisher,
}

impl ScopedLogger {
    pub fn from_directory(dir: &fio::DirectoryProxy, path: &str) -> Result<Self, Error> {
        let sink = connect_to_named_protocol_at_dir_root::<LogSinkMarker>(dir, path)?;
        let publisher = Publisher::new(
            PublisherOptions::default().wait_for_initial_interest(false).use_log_sink(sink),
        )?;
        Ok(Self { publisher })
    }
}

impl tracing::Subscriber for ScopedLogger {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        self.publisher.enabled(metadata)
    }

    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        self.publisher.new_span(span)
    }

    fn record(&self, span: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        self.publisher.record(span, values)
    }

    fn record_follows_from(&self, span: &tracing::span::Id, follows: &tracing::span::Id) {
        self.publisher.record_follows_from(span, follows)
    }

    fn event(&self, event: &tracing::Event<'_>) {
        self.publisher.event(event)
    }

    fn enter(&self, span: &tracing::span::Id) {
        self.publisher.enter(span)
    }

    fn exit(&self, span: &tracing::span::Id) {
        self.publisher.exit(span)
    }
}
