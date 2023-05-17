// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{bail, format_err, Result},
    fidl_fuchsia_dash as fdash, fidl_fuchsia_io as fio,
    futures::prelude::*,
    moniker::RelativeMoniker,
    std::io::{Read, StdoutLock, Write},
    std::str::FromStr,
    termion::raw::{IntoRawMode, RawTerminal},
};

pub enum Stdout<'a> {
    Raw(RawTerminal<StdoutLock<'a>>),
    Buffered,
}

impl std::io::Write for Stdout<'_> {
    fn flush(&mut self) -> Result<(), std::io::Error> {
        match self {
            Self::Raw(r) => r.flush(),
            Self::Buffered => std::io::stdout().flush(),
        }
    }
    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        match self {
            Self::Raw(r) => r.write(buf),
            Self::Buffered => std::io::stdout().write(buf),
        }
    }
}

impl Stdout<'_> {
    pub fn raw() -> Result<Self> {
        let stdout = std::io::stdout();

        if !termion::is_tty(&stdout) {
            bail!("interactive mode does not support piping");
        }

        // Put the host terminal into raw mode, so input characters are not echoed, streams are
        // not buffered and newlines are not changed.
        let term_out = std::io::stdout()
            .lock()
            .into_raw_mode()
            .map_err(|e| format_err!("could not set raw mode on terminal: {}", e))?;

        Ok(Self::Raw(term_out))
    }

    pub fn buffered() -> Self {
        Self::Buffered
    }
}

#[derive(Debug, PartialEq)]
pub enum DashNamespaceLayout {
    NestAllInstanceDirs,
    InstanceNamespaceIsRoot,
}

impl FromStr for DashNamespaceLayout {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "namespace" => Ok(Self::InstanceNamespaceIsRoot),
            "nested" => Ok(Self::NestAllInstanceDirs),
            _ => Err(format_err!("unknown layout (expected 'namespace' or 'nested')")),
        }
    }
}

impl Into<fdash::DashNamespaceLayout> for DashNamespaceLayout {
    fn into(self) -> fdash::DashNamespaceLayout {
        match self {
            Self::NestAllInstanceDirs => fdash::DashNamespaceLayout::NestAllInstanceDirs,
            Self::InstanceNamespaceIsRoot => fdash::DashNamespaceLayout::InstanceNamespaceIsRoot,
        }
    }
}

pub async fn connect_socket_to_stdio(socket: fidl::Socket, mut stdout: Stdout<'_>) -> Result<()> {
    let pty = fuchsia_async::Socket::from_socket(socket)?;
    let (mut read_from_pty, mut write_to_pty) = pty.split();

    // Set up a thread for forwarding stdin. Reading from stdin is a blocking operation which
    // will halt the executor if it were to run on the same thread.
    std::thread::spawn(move || {
        let mut executor = fuchsia_async::LocalExecutor::new();
        executor.run_singlethreaded(async move {
            let mut term_in = std::io::stdin().lock();
            let mut buf = [0u8; fio::MAX_BUF as usize];
            loop {
                let bytes_read = term_in.read(&mut buf)?;
                if bytes_read == 0 {
                    return Ok::<(), anyhow::Error>(());
                }
                write_to_pty.write_all(&buf[..bytes_read]).await?;
                write_to_pty.flush().await?;
            }
        })?;
        Ok::<(), anyhow::Error>(())
    });

    // In a loop, wait for the TTY to be readable and print out the bytes.
    loop {
        let mut buf = [0u8; fio::MAX_BUF as usize];
        let bytes_read = read_from_pty.read(&mut buf).await?;
        if bytes_read == 0 {
            // There are no more bytes to read. This means that the socket has been closed. This is
            // probably because the dash process has terminated.
            break;
        }
        stdout.write_all(&buf[..bytes_read])?;
        stdout.flush()?;
    }

    Ok(())
}

pub async fn explore_over_socket(
    moniker: RelativeMoniker,
    pty_server: fidl::Socket,
    tools_urls: Vec<String>,
    command: Option<String>,
    ns_layout: DashNamespaceLayout,
    launcher_proxy: &fdash::LauncherProxy,
) -> Result<()> {
    // TODO(fxbug.dev/127374) Use explore_component_over_socket once server support has rolled out
    // to local dev devices.
    launcher_proxy
        .launch_with_socket(
            &moniker.to_string(),
            pty_server,
            &tools_urls,
            command.as_deref(),
            ns_layout.into(),
        )
        .await
        .map_err(|e| format_err!("fidl error launching dash: {}", e))?
        .map_err(|e| match e {
            fdash::LauncherError::InstanceNotFound => {
                format_err!("No instance was found matching the moniker '{}'.", moniker)
            }
            fdash::LauncherError::InstanceNotResolved => format_err!(
                "{} is not resolved. Resolve the instance and retry this command",
                moniker
            ),
            e => format_err!("Unexpected error launching dash: {:?}", e),
        })?;
    Ok(())
}

pub async fn wait_for_shell_exit(launcher_proxy: &fdash::LauncherProxy) -> Result<i32> {
    // Report process errors and return the exit status.
    let mut event_stream = launcher_proxy.take_event_stream();
    match event_stream.next().await {
        Some(Ok(fdash::LauncherEvent::OnTerminated { return_code })) => Ok(return_code),
        Some(Err(e)) => Err(format_err!("OnTerminated event error: {:?}", e)),
        None => Err(format_err!("didn't receive an expected OnTerminated event")),
    }
}
