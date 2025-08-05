// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_message::MonikerWithUrl;
use fidl::endpoints::{RequestStream, ServerEnd};
use fidl_fuchsia_logger::{LogSinkMarker, LogSinkOnInitRequest};
use fuchsia_async as fasync;
use futures::channel::mpsc;
use ring_buffer::RingBuffer;
use std::sync::Arc;

/// FakeLogSink serves LogSink connections and forward any messages logged to it.
pub struct FakeLogSink {
    ring_buffer: Arc<RingBuffer>,
    _task: fasync::Task<()>,
}

impl FakeLogSink {
    /// Returns a new FakeLogSink and receiver that will have messages delivered to it.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<String>) {
        let mut reader = RingBuffer::create(ring_buffer::MAX_MESSAGE_SIZE);
        let (tx, rx) = mpsc::unbounded();

        (
            Self {
                ring_buffer: reader.clone(),
                _task: fasync::Task::spawn(async move {
                    while let Ok((_tag, bytes)) = reader.read_message().await {
                        if tx
                            .unbounded_send(
                                diagnostics_message::from_structured(
                                    MonikerWithUrl {
                                        url: "".into(),
                                        moniker: "fake-log-sink".try_into().unwrap(),
                                    },
                                    &bytes,
                                )
                                .unwrap()
                                .msg()
                                .unwrap()
                                .into(),
                            )
                            .is_err()
                        {
                            return;
                        }
                    }
                }),
            },
            rx,
        )
    }

    /// Handles the server end of the LogSink connection.
    pub fn serve(&self, server: ServerEnd<LogSinkMarker>) {
        let (iob, _) = self.ring_buffer.new_iob_writer(1).unwrap();

        server
            .into_stream()
            .control_handle()
            .send_on_init(LogSinkOnInitRequest { buffer: Some(iob), ..Default::default() })
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::FakeLogSink;
    use diagnostics_log_encoding::encode::{
        Encoder, EncoderOpts, LogEvent, MutableBuffer, WriteEventParams,
    };
    use fidl::endpoints::create_proxy;
    use fidl_fuchsia_logger::{LogSinkEvent, LogSinkOnInitRequest, MAX_DATAGRAM_LEN_BYTES};
    use futures::StreamExt;
    use std::io::Cursor;

    #[fuchsia::test(logging = false)]
    async fn log() {
        let (proxy, server) = create_proxy();
        let (fake_sink, mut rx) = FakeLogSink::new();
        fake_sink.serve(server);

        // NOTE: This can be changed to use the diagnostics client library when support for the
        // IOBuffer has been added.
        let Some(Ok(LogSinkEvent::OnInit {
            payload: LogSinkOnInitRequest { buffer: Some(iob), .. },
        })) = proxy.take_event_stream().next().await
        else {
            panic!("Expected OnInit")
        };

        let mut buf = [0u8; MAX_DATAGRAM_LEN_BYTES as _];
        let mut encoder = Encoder::new(Cursor::new(&mut buf[..]), EncoderOpts::default());
        let tags: &[&str] = &[];
        const MSG: &str = "The quick brown fox jumps over the lazy dog";
        encoder
            .write_event(WriteEventParams {
                event: LogEvent::new(&log::Record::builder().args(format_args!("{MSG}")).build()),
                tags,
                metatags: std::iter::empty(),
                pid: zx::Koid::from_raw(1),
                tid: zx::Koid::from_raw(2),
                dropped: 0,
            })
            .unwrap();
        let end = encoder.inner().cursor();
        iob.write(Default::default(), 0, &encoder.inner().get_ref()[..end]).unwrap();

        assert_eq!(rx.next().await.unwrap(), MSG);
    }
}
