// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_logger::{LogSinkRequest, LogSinkRequestStream};
use futures::prelude::*;

pub struct LogSink;

impl LogSink {
    /// Serves an instance of the `fuchsia.process.Launcher` protocol given an appropriate
    /// RequestStream. Returns when the channel backing the RequestStream is closed or an
    /// unrecoverable error, like a failure to read from the stream, occurs.
    pub async fn serve(mut stream: LogSinkRequestStream) -> Result<(), fidl::Error> {
        while let Some(req) = stream.try_next().await? {
            match req {
                LogSinkRequest::Connect { .. } => {}
                LogSinkRequest::ConnectStructured { .. } => {}
                LogSinkRequest::WaitForInterestChange { .. } => {}
            }
        }
        Ok(())
    }
}
