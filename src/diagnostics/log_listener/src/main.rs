// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(missing_docs)]

//! `log_listener` listens to messages from `fuchsia.logger.Log` and prints them to stdout and/or
//! writes them to disk.

use anyhow::Error;
use async_trait::async_trait;
use ffx_writer::{Format, MachineWriter};
use fidl_fuchsia_diagnostics::{LogSettingsMarker, StreamParameters};
use fidl_fuchsia_diagnostics_host::ArchiveAccessorMarker;
use fidl_fuchsia_sys2::RealmQueryMarker;
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at_path};
use log_command as log_utils;
use log_command::log_formatter;
use log_formatter::{
    dump_logs_from_socket as read_logs_from_socket, DefaultLogFormatter, LogEntry, Symbolize,
    Timestamp,
};
use log_utils::log_formatter::BootTimeAccessor;
use log_utils::{LogCommand, LogSubCommand};
use std::io::Write;

/// Target-side symbolizer implementation.
/// Does nothing as no symbols are available on the target.
struct Symbolizer {}

impl Symbolizer {
    fn new() -> Self {
        Self {}
    }
}

#[async_trait(?Send)]
impl Symbolize for Symbolizer {
    async fn symbolize(&self, entry: LogEntry) -> Option<LogEntry> {
        Some(entry)
    }
}

#[fuchsia_async::run_singlethreaded]
async fn main() -> Result<(), Error> {
    let (sender, receiver) = zx::Socket::create_stream();
    let proxy = connect_to_protocol::<ArchiveAccessorMarker>().unwrap();
    let realm_proxy =
        connect_to_protocol_at_path::<RealmQueryMarker>("/svc/fuchsia.sys2.RealmQuery.root")
            .unwrap();
    let log_settings = connect_to_protocol::<LogSettingsMarker>().unwrap();
    let cmd: LogCommand = argh::from_env();
    let stream_mode = if matches!(cmd.sub_command, Some(LogSubCommand::Dump(..))) {
        fidl_fuchsia_diagnostics::StreamMode::Snapshot
    } else {
        cmd.since
            .as_ref()
            .map(|value| {
                if value.is_now {
                    fidl_fuchsia_diagnostics::StreamMode::Subscribe
                } else {
                    fidl_fuchsia_diagnostics::StreamMode::SnapshotThenSubscribe
                }
            })
            .unwrap_or(fidl_fuchsia_diagnostics::StreamMode::SnapshotThenSubscribe)
    };
    proxy
        .stream_diagnostics(
            &StreamParameters {
                data_type: Some(fidl_fuchsia_diagnostics::DataType::Logs),
                stream_mode: Some(stream_mode),
                format: Some(fidl_fuchsia_diagnostics::Format::Json),
                client_selector_configuration: Some(
                    fidl_fuchsia_diagnostics::ClientSelectorConfiguration::SelectAll(true),
                ),
                ..Default::default()
            },
            sender,
        )
        .await
        .unwrap();
    let boot_ts = Timestamp::from_nanos(
        fuchsia_runtime::utc_time().into_nanos() - zx::BootInstant::get().into_nanos(),
    );
    let mut formatter = DefaultLogFormatter::<MachineWriter<LogEntry>>::new_from_args(
        &cmd,
        MachineWriter::new(if cmd.json { Some(Format::Json) } else { None }),
    );
    cmd.maybe_set_interest(&log_settings, &realm_proxy).await?;
    formatter.set_boot_timestamp(boot_ts);
    let _ = read_logs_from_socket(
        fuchsia_async::Socket::from_socket(receiver),
        &mut formatter,
        &Symbolizer::new(),
        true,
    )
    .await;
    let _ = std::io::stdout().flush();
    Ok(())
}
