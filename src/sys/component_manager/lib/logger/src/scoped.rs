// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_log::{PublishError, Publisher, PublisherOptions};
use fidl_fuchsia_logger as flogger;
use thiserror::Error;

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

impl log::Log for ScopedLogger {
    #[inline]
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.publisher.enabled(metadata)
    }

    #[inline]
    fn log(&self, record: &log::Record<'_>) {
        self.publisher.log(record);
    }

    #[inline]
    fn flush(&self) {
        self.publisher.flush();
    }
}
