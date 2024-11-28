// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::identity::ComponentIdentity;
use crate::logs::container::CursorItem;
use crate::logs::error::LogsError;
use crate::logs::repository::LogsRepository;
use diagnostics_log_encoding::encode::{Encoder, MutableBuffer, ResizableBuffer};
use diagnostics_log_encoding::{Header, FXT_HEADER_SIZE};
use fidl::endpoints::{ControlHandle, DiscoverableProtocolMarker};
use fidl_fuchsia_diagnostics::StreamMode;
use futures::{AsyncWriteExt, Stream, StreamExt};
use std::borrow::Cow;
use std::io::Cursor;
use std::sync::Arc;
use tracing::warn;
use zerocopy::FromBytes;
use {fidl_fuchsia_diagnostics as fdiagnostics, fuchsia_async as fasync, fuchsia_trace as ftrace};

pub struct LogStreamServer {
    /// The repository holding the logs.
    logs_repo: Arc<LogsRepository>,

    /// Scope in which we spawn all of the server tasks.
    scope: fasync::Scope,
}

impl LogStreamServer {
    pub fn new(logs_repo: Arc<LogsRepository>, scope: fasync::Scope) -> Self {
        Self { logs_repo, scope }
    }

    /// Spawn a task to handle requests from components reading the shared log.
    pub fn spawn(&self, stream: fdiagnostics::LogStreamRequestStream) {
        let logs_repo = Arc::clone(&self.logs_repo);
        let scope = self.scope.to_handle();
        self.scope.spawn(async move {
            if let Err(e) = Self::handle_requests(logs_repo, stream, scope).await {
                warn!("error handling Log requests: {}", e);
            }
        });
    }

    /// Handle requests to `fuchsia.diagnostics.LogStream`. All request types read the
    /// whole backlog from memory, `DumpLogs(Safe)` stops listening after that.
    async fn handle_requests(
        logs_repo: Arc<LogsRepository>,
        mut stream: fdiagnostics::LogStreamRequestStream,
        scope: fasync::ScopeHandle,
    ) -> Result<(), LogsError> {
        while let Some(request) = stream.next().await {
            let request = request.map_err(|source| LogsError::HandlingRequests {
                protocol: fdiagnostics::LogStreamMarker::PROTOCOL_NAME,
                source,
            })?;

            match request {
                fdiagnostics::LogStreamRequest::Connect { socket, opts, .. } => {
                    let logs = logs_repo.logs_cursor_raw(
                        opts.mode.unwrap_or(StreamMode::SnapshotThenSubscribe),
                        ftrace::Id::random(),
                    );
                    let opts = ExtendRecordOpts::from(opts);
                    scope.spawn(Self::stream_logs(fasync::Socket::from_socket(socket), logs, opts));
                }
                fdiagnostics::LogStreamRequest::_UnknownMethod {
                    ordinal,
                    method_type,
                    control_handle,
                    ..
                } => {
                    warn!(ordinal, ?method_type, "Unknown request. Closing connection");
                    control_handle.shutdown_with_epitaph(zx::Status::UNAVAILABLE);
                }
            }
        }
        Ok(())
    }

    async fn stream_logs(
        mut socket: fasync::Socket,
        mut logs: impl Stream<Item = CursorItem> + Unpin,
        opts: ExtendRecordOpts,
    ) {
        while let Some(CursorItem { rolled_out, message, identity }) = logs.next().await {
            let response = extend_fxt_record(message.bytes(), identity.as_ref(), rolled_out, &opts);
            let result = socket.write_all(&response).await;
            if result.is_err() {
                // Assume an error means the peer closed for now.
                break;
            }
        }
    }
}

#[derive(Default)]
struct ExtendRecordOpts {
    moniker: bool,
    component_url: bool,
    rolled_out: bool,
}

impl ExtendRecordOpts {
    fn should_extend(&self) -> bool {
        let Self { moniker, component_url, rolled_out } = self;
        *moniker || *component_url || *rolled_out
    }
}

impl From<fdiagnostics::LogStreamOptions> for ExtendRecordOpts {
    fn from(opts: fdiagnostics::LogStreamOptions) -> Self {
        let fdiagnostics::LogStreamOptions {
            include_moniker,
            include_component_url,
            include_rolled_out,
            mode: _,
            __source_breaking: _,
        } = opts;
        Self {
            moniker: include_moniker.unwrap_or(false),
            component_url: include_component_url.unwrap_or(false),
            rolled_out: include_rolled_out.unwrap_or(false),
        }
    }
}

fn extend_fxt_record<'a>(
    fxt_record: &'a [u8],
    identity: &ComponentIdentity,
    rolled_out: u64,
    opts: &ExtendRecordOpts,
) -> Cow<'a, [u8]> {
    if !opts.should_extend() {
        return Cow::Borrowed(fxt_record);
    }

    let mut cursor = Cursor::new(ResizableBuffer::from(vec![0; fxt_record.len()]));
    cursor.put_slice(fxt_record).expect("must fit");

    let mut metadata_arguments = Encoder::new(cursor, Default::default());
    if opts.moniker {
        metadata_arguments
            .write_raw_argument(fdiagnostics::MONIKER_ARG_NAME, identity.moniker.to_string())
            .expect("infallible");
    }
    if opts.component_url {
        metadata_arguments
            .write_raw_argument(fdiagnostics::COMPONENT_URL_ARG_NAME, identity.url.as_ref())
            .expect("infallible");
    }
    if opts.rolled_out && rolled_out > 0 {
        metadata_arguments
            .write_raw_argument(fdiagnostics::ROLLED_OUT_ARG_NAME, rolled_out)
            .expect("infallible");
    }

    let buffer = metadata_arguments.take();
    let length = buffer.cursor();
    let mut buffer = buffer.into_inner().into_inner();

    let (header, _) = Header::mut_from_prefix(&mut buffer[..FXT_HEADER_SIZE])
        .expect("we validate the header when ingesting");
    header.set_len(length);
    buffer.resize(length, 0);
    Cow::Owned(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_log_encoding::encode::{EncoderOpts, TestRecord, WriteEventParams};
    use diagnostics_log_encoding::parse::parse_record;
    use diagnostics_log_encoding::{Argument, Severity};
    use test_case::test_case;

    #[test_case(ExtendRecordOpts::default(), vec![] ; "no_additional_metadata")]
    #[test_case(
        ExtendRecordOpts { moniker: true, ..Default::default() },
        vec![Argument::other(fdiagnostics::MONIKER_ARG_NAME, "UNKNOWN")]
        ; "with_moniker")]
    #[test_case(
        ExtendRecordOpts { component_url: true, ..Default::default() },
        vec![Argument::other(fdiagnostics::COMPONENT_URL_ARG_NAME, "fuchsia-pkg://UNKNOWN")]
        ; "with_url")]
    #[test_case(
        ExtendRecordOpts { rolled_out: true, ..Default::default() },
        vec![Argument::other(fdiagnostics::ROLLED_OUT_ARG_NAME, 42u64)]
        ; "with_rolled_out")]
    #[fuchsia::test]
    fn extend_record_with_metadata(opts: ExtendRecordOpts, arguments: Vec<Argument<'static>>) {
        let mut encoder = Encoder::new(Cursor::new([0u8; 4096]), EncoderOpts::default());
        encoder
            .write_event(WriteEventParams::<_, &str, _> {
                event: TestRecord {
                    severity: Severity::Warn as u8,
                    timestamp: zx::BootInstant::from_nanos(1234567890),
                    file: Some("foo.rs"),
                    line: Some(123),
                    record_arguments: vec![Argument::tag("hello"), Argument::message("testing")],
                },
                tags: &[],
                metatags: std::iter::empty(),
                pid: zx::Koid::from_raw(1),
                tid: zx::Koid::from_raw(2),
                dropped: 10,
            })
            .expect("wrote event");

        let length = encoder.inner().cursor();
        let original_record_bytes = &encoder.inner().get_ref()[..length];
        let (mut expected_record, _) = parse_record(original_record_bytes).unwrap();

        let extended_record_bytes =
            extend_fxt_record(original_record_bytes, &ComponentIdentity::unknown(), 42, &opts);
        let (extended_record, _) = parse_record(&extended_record_bytes).unwrap();

        // The expected record is the original record plus the additional arguments that were
        // requested.
        expected_record.arguments.extend(arguments);
        assert_eq!(extended_record, expected_record);
    }
}
