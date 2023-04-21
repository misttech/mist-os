// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{commands::types::*, types::Error},
    argh::FromArgs,
    async_trait::async_trait,
    diagnostics_data::{Logs, LogsData},
    serde::Serialize,
    std::fmt,
};

/// Prints the logs.
#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "logs")]
pub struct LogsCommand {
    #[argh(option)]
    /// A selector specifying what `fuchsia.diagnostics.ArchiveAccessor` to connect to.
    /// The selector will be in the form of:
    /// <moniker>:<directory>:fuchsia.diagnostics.ArchiveAccessorName
    ///
    /// Typically this is the output of `iquery list-accessors`.
    ///
    /// For example: `bootstrap/archivist:expose:fuchsia.diagnostics.FeedbackArchiveAccessor`
    /// means that the command will connect to the `FeedbackArchiveAccecssor`
    /// exposed by `bootstrap/archivist`.
    pub accessor: Option<String>,
}

#[derive(Serialize)]
pub struct LogsResult(Vec<LogsData>);

impl fmt::Display for LogsResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for log in self.0.iter() {
            writeln!(f, "{}", log)?;
        }
        Ok(())
    }
}

#[async_trait]
impl Command for LogsCommand {
    type Result = LogsResult;

    async fn execute<P: DiagnosticsProvider>(&self, provider: &P) -> Result<Self::Result, Error> {
        let mut results = provider.snapshot::<Logs>(&self.accessor, &[]).await?;
        for result in results.iter_mut() {
            if let Some(hierarchy) = &mut result.payload {
                hierarchy.sort();
            }
        }
        Ok(LogsResult(results))
    }
}
