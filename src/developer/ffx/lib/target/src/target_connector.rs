// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use async_channel::{Receiver, Sender};
use compat_info::CompatibilityInfo;
use ffx_ssh::parse::HostAddr;
use fuchsia_async::Task;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, BufReader};

pub(crate) const BUFFER_SIZE: usize = 65536;

#[derive(thiserror::Error, Debug)]
pub enum TargetConnectionError {
    /// A non-recoverable error. Any errors converted via From<> trait are automatically converted
    /// to this value.
    #[error("encountered fatal error during connect {0}")]
    Fatal(#[from] anyhow::Error),
    /// A potentially recoverable error, signalling the invoker that they may reattempt to connect.
    /// This error must be returned explicitly rather than converted into via the `?` operator.
    #[error("encountered potentially recoverable error during connect: {0}")]
    NonFatal(#[source] anyhow::Error),
}

/// Collection of connections established by a connector.
pub enum TargetConnection {
    Overnet(OvernetConnection),
    FDomain(FDomainConnection),
    Both(FDomainConnection, OvernetConnection),
}

pub trait TargetConnector: Debug {
    /// A debugging label for the type of connection. Intended to be for error formatting.
    const CONNECTION_TYPE: &'static str;

    /// Attempts a connection to a target. This function, if it fails and
    /// returns a `NonFatal` error, should be capable of running again. It will
    /// be the caller's responsibility to determine whether and how often to
    /// re-attempt connecting when receiving a NonFatal error.
    fn connect(&mut self) -> impl Future<Output = Result<TargetConnection, TargetConnectionError>>;

    fn device_address(&self) -> Option<std::net::SocketAddr> {
        None
    }
}

#[allow(unused)]
pub struct FDomainConnection {
    // Currently because of the implementation of ffx_ssh::parse::parse_ssh_output's
    // implementation, this needs to be a buffered reader.
    pub(crate) output: Box<dyn AsyncBufRead + Unpin + Send>,
    pub(crate) input: Box<dyn AsyncWrite + Unpin + Send>,
    pub(crate) errors: Receiver<anyhow::Error>,
    pub(crate) main_task: Option<Task<()>>,
}

pub struct OvernetConnection {
    // Currently because of the implementation of ffx_ssh::parse::parse_ssh_output's
    // implementation, this needs to be a buffered reader.
    pub(crate) output: Box<dyn AsyncBufRead + Unpin>,
    pub(crate) input: Box<dyn AsyncWrite + Unpin>,
    pub(crate) errors: Receiver<anyhow::Error>,
    pub(crate) compat: Option<CompatibilityInfo>,
    pub(crate) main_task: Option<Task<()>>,
    pub(crate) ssh_host_address: Option<HostAddr>,
}

impl OvernetConnection {
    /// Runs an overnet connection to completion.
    ///
    /// Arguments:
    ///
    /// -- recv: The overnet node receiver pipe.
    /// -- send: The overnet sender pipe.
    pub(crate) fn run<'a, W, R>(
        self,
        mut recv: W,
        send: R,
        error_sender: Sender<anyhow::Error>,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>>
    where
        R: AsyncRead + Unpin + Sized + 'a,
        W: AsyncWrite + Unpin + Sized + 'a,
    {
        let err_clone = error_sender.clone();
        let output = self.output;
        let copy_in = async move {
            if let Err(e) =
                tokio::io::copy_buf(&mut BufReader::with_capacity(BUFFER_SIZE, output), &mut recv)
                    .await
            {
                let _ =
                    err_clone.send(anyhow::anyhow!("overnet connection read failure: {e:?}")).await;
            };
        };
        let err_clone = error_sender.clone();
        let mut input = self.input;
        let copy_out = async move {
            if let Err(e) =
                tokio::io::copy_buf(&mut BufReader::with_capacity(BUFFER_SIZE, send), &mut input)
                    .await
            {
                let _ = err_clone
                    .send(anyhow::anyhow!("overnet connection write failure: {e:?}"))
                    .await;
            }
        };
        let err_clone = error_sender.clone();
        let errors = self.errors;
        let error_reader = async move {
            while let Ok(err) = errors.recv().await {
                if err_clone.send(err).await.is_err() {
                    break;
                }
            }
        };
        let main_task = async move {
            let copy_fut = futures_lite::future::or(copy_in, copy_out);
            let overall_fut = futures_lite::future::zip(copy_fut, error_reader);
            if let Some(t) = self.main_task {
                let _ = futures_lite::future::zip(overall_fut, t).await;
            } else {
                let _ = overall_fut.await;
            }
            error_sender.close();
        };

        Box::pin(main_task)
    }
}
