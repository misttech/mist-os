// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod usb;

use crate::usb::listen_for_usb_devices;
use anyhow::Context as ErrorContext;
use anyhow::{bail, format_err, Error};
use argh::FromArgs;
use async_net::unix::{UnixListener, UnixStream};
use fuchsia_async::Task;
use fuchsia_async::TimeoutExt;
use futures::channel::mpsc::unbounded;
use futures::prelude::*;
use hoist::Hoist;
use overnet_core::AscenddClientRouting;
use std::io::{
    ErrorKind::{self, TimedOut},
    Write,
};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

#[cfg(not(target_os = "fuchsia"))]
pub use hoist::default_ascendd_path;

#[cfg(not(target_os = "fuchsia"))]
use hoist::CIRCUIT_ID;

#[derive(FromArgs, Default)]
/// daemon to lift a non-Fuchsia device into Overnet.
pub struct Opt {
    #[argh(option, long = "sockpath")]
    /// path to the ascendd socket.
    /// If not provided, this will default to a new socket-file in /tmp.
    pub sockpath: Option<PathBuf>,

    #[argh(option, long = "serial")]
    /// selector for which serial devices to communicate over.
    /// Could be 'none' to not communcate over serial, 'all' to query all serial devices and try to
    /// communicate with them, or a path to a serial device to communicate over *that* device.
    /// If not provided, this will default to 'none'.
    pub serial: Option<String>,

    #[argh(option, long = "client-routing", default = "true")]
    /// route Ascendd clients to each other. Can be turned off to avoid scaling issues
    /// when multiple ffxs are run concurrently, with no requirement to have
    /// them interact. (Normally set to false iff run as ffx daemon.)
    pub client_routing: bool,

    #[argh(option)]
    /// paths to other ascendds which we will connect this ascendd to, causing
    /// both to be part of the same network.
    pub link: Vec<PathBuf>,

    #[argh(option, default = "false")]
    /// allow ascendd to scan for USB devices and connect to them automatically.
    pub usb: bool,
}

#[derive(Debug)]
pub struct Ascendd {
    task: Task<Result<(), Error>>,
}

impl Ascendd {
    // Initializes and binds ascendd socket, but does not accept connections yet.
    pub async fn prime(mut opt: Opt, hoist: &Hoist) -> Result<impl FnOnce() -> Self, Error> {
        let usb = opt.usb;
        let link = std::mem::replace(&mut opt.link, vec![]);
        let (sockpath, client_routing, incoming) = bind_listener(opt, hoist).await?;
        let hoist = hoist.clone();
        Ok(move || Self {
            task: Task::spawn(run_ascendd(hoist, sockpath, incoming, client_routing, usb, link)),
        })
    }

    pub async fn new(opt: Opt, hoist: &Hoist) -> Result<Self, Error> {
        Self::prime(opt, hoist).await.map(|f| f())
    }
}

impl Future for Ascendd {
    type Output = Result<(), Error>;

    fn poll(mut self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        self.task.poll_unpin(ctx)
    }
}

/// Error returned from [`run_stream`].
#[derive(Debug)]
pub enum RunStreamError {
    Circuit(circuit::Error),
    EarlyHangup(std::io::Error),
    Unsupported,
}

/// Run an ascendd server on the given stream IOs identified by the given labels
/// and paths, to completion.
pub async fn run_stream<'a>(
    node: Arc<overnet_core::Router>,
    rx: &'a mut (dyn AsyncRead + Unpin + Send),
    tx: &'a mut (dyn AsyncWrite + Unpin + Send),
) -> Result<(), RunStreamError> {
    let mut id = [0; 8];

    if let Err(e) = rx.read_exact(&mut id).await {
        return Err(RunStreamError::EarlyHangup(e));
    }

    if id == CIRCUIT_ID {
        let (errors_sender, errors) = unbounded();
        futures::future::join(
            circuit::multi_stream::multi_stream_node_connection_to_async(
                node.circuit_node(),
                rx,
                tx,
                true,
                circuit::Quality::LOCAL_SOCKET,
                errors_sender,
                "ascendd client".to_owned(),
            ),
            errors
                .map(|e| {
                    tracing::warn!("An ascendd client circuit stream failed: {e:?}");
                })
                .collect::<()>(),
        )
        .map(|(result, ())| result)
        .await
        .map_err(RunStreamError::Circuit)
    } else {
        Err(RunStreamError::Unsupported)
    }
}

/// Run a connection to another ascendd server.
/// and paths, to completion.
pub async fn run_linked_ascendd<'a>(
    node: Arc<overnet_core::Router>,
    rx: &'a mut (dyn AsyncRead + Unpin + Send),
    tx: &'a mut (dyn AsyncWrite + Unpin + Send),
) {
    let (errors_sender, errors) = unbounded();
    if let Err(e) = futures::future::join(
        async move {
            tx.write_all(&CIRCUIT_ID).await?;
            circuit::multi_stream::multi_stream_node_connection_to_async(
                node.circuit_node(),
                rx,
                tx,
                false,
                circuit::Quality::LOCAL_SOCKET,
                errors_sender,
                "ascendd client".to_owned(),
            )
            .await
        },
        errors
            .map(|e| {
                tracing::warn!("An ascendd linking circuit stream failed: {e:?}");
            })
            .collect::<()>(),
    )
    .map(|(result, ())| result)
    .await
    {
        tracing::debug!(err = ?e, "Link to other ascendd closed");
    }
}

async fn bind_listener(
    opt: Opt,
    hoist: &Hoist,
) -> Result<(PathBuf, AscenddClientRouting, UnixListener), Error> {
    let Opt { sockpath, serial: _, client_routing, usb: _, link: _ } = opt;
    let sockpath = sockpath.unwrap_or(default_ascendd_path());

    let client_routing =
        if client_routing { AscenddClientRouting::Enabled } else { AscenddClientRouting::Disabled };
    tracing::debug!(
        node_id = hoist.node().node_id().0,
        "starting ascendd on {}",
        sockpath.display(),
    );

    let incoming = loop {
        let safe_socket_path = hoist::short_socket_path(&sockpath)?;
        match UnixListener::bind(&safe_socket_path) {
            Ok(listener) => {
                break listener;
            }
            Err(_) => match UnixStream::connect(&safe_socket_path)
                .on_timeout(Duration::from_secs(1), || {
                    Err(std::io::Error::new(TimedOut, format_err!("connecting to ascendd socket")))
                })
                .await
            {
                Ok(_) => {
                    tracing::error!(
                        "another ascendd is already listening at {}",
                        sockpath.display()
                    );
                    bail!("another ascendd is aleady listening at {}!", sockpath.display());
                }
                Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
                    tracing::info!(
                        "trying to clean up stale ascendd socket at {} (error: {e:?})",
                        sockpath.display()
                    );
                    std::fs::remove_file(&sockpath)?;
                }
                Err(e) => {
                    tracing::info!("An unexpected error occurred while trying to bind to the ascendd socket at {}: {e:?}", sockpath.display());
                    bail!(
                        "unexpected error while trying to bind to ascendd socket at {}: {e}",
                        sockpath.display()
                    );
                }
            },
        }
    };

    // as this file is purely advisory, we won't fail for any error, but we can log it.
    if let Err(e) = write_pidfile(&sockpath, std::process::id()) {
        tracing::warn!("failed to write pidfile alongside {}: {e:?}", sockpath.display());
    }
    Ok((sockpath, client_routing, incoming))
}

/// Writes a pid file alongside the socketpath so we know what pid last successfully tried to
/// create a socket there.
fn write_pidfile(sockpath: &Path, pid: u32) -> anyhow::Result<()> {
    let in_dir = sockpath.parent().context("No parent directory for socket path")?;
    let mut pidfile = tempfile::NamedTempFile::new_in(in_dir)?;
    write!(pidfile, "{pid}")?;
    pidfile.persist(sockpath.with_extension("pid"))?;
    Ok(())
}

async fn run_ascendd(
    hoist: Hoist,
    sockpath: PathBuf,
    incoming: UnixListener,
    client_routing: AscenddClientRouting,
    usb: bool,
    link: Vec<PathBuf>,
) -> Result<(), Error> {
    let node = hoist.node();
    node.set_implementation(fidl_fuchsia_overnet_protocol::Implementation::Ascendd);
    node.set_client_routing(client_routing);

    tracing::debug!("ascendd listening to socket {}", sockpath.display());

    let hoist = &hoist;

    futures::future::try_join3(
        futures::stream::iter(link.into_iter().map(Ok)).try_for_each_concurrent(None, |path| {
            let node = Arc::clone(&node);
            async move {
                let sock = loop {
                    match UnixStream::connect(&path).await {
                        Ok(sock) => break sock,
                        Err(e) => {
                            tracing::debug!(path = ?path.display(), error = ?e,
                            "Linking to socket failed. Retrying..");
                            fuchsia_async::Timer::new(std::time::Duration::from_secs(1)).await
                        }
                    }
                };
                let (mut rx, mut tx) = sock.split();
                run_linked_ascendd(node, &mut rx, &mut tx).await;
                Ok(())
            }
        }),
        async move {
            if usb {
                listen_for_usb_devices(Arc::downgrade(&hoist.node())).await
            } else {
                Ok(())
            }
        },
        async move {
            incoming
                .incoming()
                .for_each_concurrent(None, |stream| async move {
                    match stream {
                        Ok(stream) => {
                            let (mut rx, mut tx) = stream.split();
                            if let Err(e) = run_stream(hoist.node(), &mut rx, &mut tx).await {
                                match e {
                                    RunStreamError::Circuit(circuit::Error::ConnectionClosed(
                                        reason,
                                    )) => {
                                        tracing::debug!("Circuit connection closed: {reason:?}")
                                    }
                                    RunStreamError::Circuit(other) => {
                                        tracing::warn!("Failed serving socket: {other:?}")
                                    }
                                    RunStreamError::EarlyHangup(e) => {
                                        tracing::debug!("Socket hung up early: {e:?}")
                                    }
                                    RunStreamError::Unsupported => {
                                        tracing::warn!("Socket was not a circuit connection.")
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed starting socket: {:?}", e);
                        }
                    }
                })
                .await;
            Ok(())
        },
    )
    .await
    .map(drop)
}
