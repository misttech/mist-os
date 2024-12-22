// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_log::SeverityExt;
use fidl_fuchsia_diagnostics::Severity;
use fuchsia_sync::RwLock;
use std::sync::Arc;
use tracing::{Metadata, Subscriber};
use tracing_subscriber::Layer;

pub struct KlogSeverityFilter {
    min_severity: Arc<RwLock<Severity>>,
}

impl KlogSeverityFilter {
    pub fn set_severity(&self, severity: Severity) {
        *self.min_severity.write() = severity;
    }
}

impl Default for KlogSeverityFilter {
    fn default() -> Self {
        Self { min_severity: Arc::new(RwLock::new(Severity::Info)) }
    }
}

impl<S: Subscriber> Layer<S> for KlogSeverityFilter {
    /// Always returns `sometimes` so that we can later change the filter on the fly.
    fn register_callsite(
        &self,
        _metadata: &'static Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }

    fn enabled(
        &self,
        metadata: &Metadata<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        let min_severity = self.min_severity.read();
        metadata.severity() >= *min_severity
    }
}
