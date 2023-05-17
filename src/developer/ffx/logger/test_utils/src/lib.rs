// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl::{endpoints::ServerEnd, prelude::*};
use fidl_fuchsia_developer_remotecontrol::{
    ArchiveIteratorEntry, ArchiveIteratorError, ArchiveIteratorMarker, ArchiveIteratorRequest,
    DiagnosticsData, InlineData,
};
use futures::TryStreamExt;
use std::sync::Arc;

pub struct FakeArchiveIteratorResponse {
    // Note that these are _all_ mutually exclusive.
    values: Vec<String>,
    iterator_error: Option<ArchiveIteratorError>,
    should_fidl_error: bool,
}

impl FakeArchiveIteratorResponse {
    pub fn new_with_values(values: Vec<String>) -> Self {
        Self { values, iterator_error: None, should_fidl_error: false }
    }

    pub fn new_with_error(err: ArchiveIteratorError) -> Self {
        Self { values: vec![], iterator_error: Some(err), should_fidl_error: false }
    }

    pub fn new_with_fidl_error() -> Self {
        Self { values: vec![], iterator_error: None, should_fidl_error: true }
    }
}

fn encode_diagnostics_data(data: String, use_socket: bool) -> DiagnosticsData {
    if use_socket {
        let (local, remote) = fuchsia_async::emulated_handle::Socket::create_stream();
        local.write(data.as_bytes()).unwrap();
        DiagnosticsData::Socket(remote)
    } else {
        DiagnosticsData::Inline(InlineData { data, truncated_chars: 0 })
    }
}

#[derive(Clone)]
pub struct ArchiveIteratorParameters {
    pub responses: Arc<Vec<FakeArchiveIteratorResponse>>,
    pub legacy_format: bool,
    pub use_socket: bool,
}

pub fn setup_fake_archive_iterator(
    server_end: ServerEnd<ArchiveIteratorMarker>,
    parameters: ArchiveIteratorParameters,
) -> Result<()> {
    let mut stream = server_end.into_stream()?;
    fuchsia_async::Task::local(async move {
        let mut iter = parameters.responses.iter();
        while let Ok(Some(req)) = stream.try_next().await {
            match req {
                ArchiveIteratorRequest::GetNext { responder } => match iter.next() {
                    Some(FakeArchiveIteratorResponse {
                        values,
                        iterator_error,
                        should_fidl_error,
                    }) => {
                        if let Some(err) = iterator_error {
                            responder.send(Err(*err)).unwrap();
                        } else if *should_fidl_error {
                            responder.control_handle().shutdown();
                        } else {
                            responder
                                .send(Ok(values
                                    .into_iter()
                                    .map(|s| {
                                        if parameters.legacy_format {
                                            ArchiveIteratorEntry {
                                                data: Some(s.clone()),
                                                truncated_chars: Some(0),
                                                ..Default::default()
                                            }
                                        } else {
                                            ArchiveIteratorEntry {
                                                diagnostics_data: Some(encode_diagnostics_data(
                                                    s.clone(),
                                                    parameters.use_socket,
                                                )),
                                                ..Default::default()
                                            }
                                        }
                                    })
                                    .collect()))
                                .unwrap()
                        }
                    }
                    None => responder.control_handle().shutdown(),
                },
            }
        }
    })
    .detach();
    Ok(())
}
