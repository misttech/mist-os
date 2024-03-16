// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use fidl_fuchsia_io as fio;
use fuchsia_async as fasync;
use fuchsia_fs::directory::readdir;
use futures::future::{FutureExt, LocalBoxFuture};
use futures::io::{AsyncWrite, AsyncWriteExt as _};
use futures::stream::StreamExt;
use playground::interpreter::Interpreter;
use playground::value::{InUseHandle, PlaygroundValue, Value, ValueExt};
use std::io;
use std::task::Poll;

/// Given a fuchsia.io/Directory channel that has come to us as a value, print a
/// directory listing.
async fn display_dir<W: AsyncWrite + Unpin>(mut writer: W, handle: InUseHandle) -> io::Result<()> {
    let (dir, server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>().unwrap();
    let server = fasync::Channel::from_channel(server.into_channel());
    let _task = fasync::Task::spawn(futures::future::poll_fn(move |ctx| -> Poll<Result<()>> {
        let mut handle_read_bytes = Vec::new();
        let mut handle_read_handles = Vec::new();
        let mut server_read_bytes = Vec::new();
        let mut server_read_handles = Vec::new();
        loop {
            let read_handle = handle
                .poll_read_channel_etc(ctx, &mut handle_read_bytes, &mut handle_read_handles)
                .map_err(|x| anyhow!("{x:?}"))?;
            let read_server =
                server.read_etc(ctx, &mut server_read_bytes, &mut server_read_handles)?;
            if read_handle.is_ready() {
                let mut handle_read_handles = handle_read_handles
                    .drain(..)
                    .map(|x| fidl::HandleDisposition {
                        handle_op: fidl::HandleOp::Move(x.handle),
                        object_type: x.object_type,
                        rights: x.rights,
                        result: fidl::Status::OK,
                    })
                    .collect::<Vec<_>>();
                server.write_etc(&handle_read_bytes, &mut handle_read_handles)?;
            }
            if read_server.is_ready() {
                let mut server_read_handles = server_read_handles
                    .drain(..)
                    .map(|x| fidl::HandleDisposition {
                        handle_op: fidl::HandleOp::Move(x.handle),
                        object_type: x.object_type,
                        rights: x.rights,
                        result: fidl::Status::OK,
                    })
                    .collect::<Vec<_>>();
                handle
                    .write_channel_etc(&server_read_bytes, &mut server_read_handles)
                    .map_err(|x| anyhow!("{x:?}"))?;
            }

            if read_server.is_pending() && read_handle.is_pending() {
                return Poll::Pending;
            }
            handle_read_bytes.clear();
            server_read_bytes.clear();
        }
    }));

    match readdir(&dir).await {
        Ok(items) => {
            for entry in items {
                writer.write_all(format!("{} {:?}\n", entry.name, entry.kind).as_bytes()).await?;
            }
        }
        Err(e) => writer.write_all(format!("Err: {:#}\n", e).as_bytes()).await?,
    }
    Ok(())
}

/// Display a result from running a command in a prettified way.
pub fn display_result<'a, W: AsyncWrite + Unpin + 'a>(
    writer: &'a mut W,
    result: playground::error::Result<Value>,
    interpreter: &'a Interpreter,
) -> LocalBoxFuture<'a, io::Result<()>> {
    async move {
        match result {
            Ok(Value::OutOfLine(PlaygroundValue::Iterator(x))) => {
                let mut stream = interpreter.replayable_iterator_to_stream(x);

                while let Some(x) = stream.next().await {
                    display_result(writer, x, interpreter).await?;
                }
            }
            Ok(x) if x.is_client("fuchsia.io/Directory") => {
                display_dir(writer, x.to_in_use_handle().unwrap()).await?
            }
            Ok(x) => writer.write_all(format!("{}\n", x).as_bytes()).await?,
            Err(x) => writer.write_all(format!("Err: {:#}\n", x).as_bytes()).await?,
        }
        Ok(())
    }
    .boxed_local()
}
