// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Result};
use compat_info::{CompatibilityInfo, ConnectionInfo};
use ffx_config::logging::LogDirHandling;
use ffx_config::EnvironmentContext;
use fuchsia_async::TimeoutExt;
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};

const BUFSIZE: usize = 1024;
pub struct LineBuffer {
    buffer: [u8; BUFSIZE],
    pos: usize,
}

// 1K should be enough for the initial line, which just looks something like
//    "++ 192.168.1.1 1234 10.0.0.1 22 ++\n"
impl LineBuffer {
    pub fn new() -> Self {
        Self { buffer: [0; BUFSIZE], pos: 0 }
    }

    pub fn line(&self) -> &[u8] {
        &self.buffer[..self.pos]
    }
}

impl ToString for LineBuffer {
    fn to_string(&self) -> String {
        String::from_utf8_lossy(self.line()).into()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ParseSshConnectionError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Parse error: {:?}", .0)]
    Parse(String),
    #[error("Unexpected EOF: {:?}", .0)]
    UnexpectedEOF(String),
    #[error("Read-line timeout")]
    Timeout,
}

#[tracing::instrument(skip(lb, rdr))]
pub async fn read_ssh_line<R: AsyncRead + Unpin>(
    lb: &mut LineBuffer,
    rdr: &mut R,
) -> std::result::Result<String, ParseSshConnectionError> {
    loop {
        // We're reading a byte at a time, which would be bad if we were doing it a lot,
        // but it's only used for stderr (which should normally not produce much data),
        // and the first line of stdout.
        let mut b = [0u8];
        let n = rdr.read(&mut b[..]).await.map_err(ParseSshConnectionError::Io)?;
        let b = b[0];
        if n == 0 {
            return Err(ParseSshConnectionError::UnexpectedEOF(lb.to_string()));
        }
        lb.buffer[lb.pos] = b;
        lb.pos += 1;
        if lb.pos >= lb.buffer.len() {
            return Err(ParseSshConnectionError::Parse(format!(
                "Buffer full: {:?}...",
                &lb.buffer[..64]
            )));
        }
        if b == b'\n' {
            let s = lb.to_string();
            // Clear for next read
            lb.pos = 0;
            return Ok(s);
        }
    }
}

#[tracing::instrument(skip(rdr))]
async fn read_ssh_line_with_timeouts<R: AsyncBufRead + Unpin>(
    rdr: &mut R,
) -> Result<String, ParseSshConnectionError> {
    let mut time = 0;
    let wait_time = 2;
    let mut lb = LineBuffer::new();
    loop {
        match read_ssh_line(&mut lb, rdr)
            .on_timeout(Duration::from_secs(wait_time), || Err(ParseSshConnectionError::Timeout))
            .await
        {
            Ok(s) => {
                return Ok(s);
            }
            Err(ParseSshConnectionError::Timeout) => {
                time += wait_time;
                tracing::debug!("No line after {time}, line so far: {:?}", lb.line());
            }
            Err(e) => {
                return Err(e);
            }
        }
    }
}

fn parse_ssh_connection_legacy(
    line: &str,
) -> std::result::Result<(String, Option<CompatibilityInfo>), ParseSshConnectionError> {
    let mut parts = line.split(" ");
    // The first part should be our anchor.
    match parts.next() {
        Some("++") => {}
        Some(_) | None => {
            tracing::error!("Failed to read first anchor: {line}");
            return Err(ParseSshConnectionError::Parse(line.into()));
        }
    }

    // SSH_CONNECTION identifies the client and server ends of the connection.
    // The variable contains four space-separated values: client IP address,
    // client port number, server IP address, and server port number.
    // This is left as a string since std::net::IpAddr does not support string scope_ids.
    let client_address = if let Some(part) = parts.next() {
        part
    } else {
        tracing::error!("Failed to read client_address: {line}");
        return Err(ParseSshConnectionError::Parse(line.into()));
    };

    // Followed by the client port.
    let _client_port = if let Some(part) = parts.next() {
        part
    } else {
        tracing::error!("Failed to read port: {line}");
        return Err(ParseSshConnectionError::Parse(line.into()));
    };

    // Followed by the server address.
    let _server_address = if let Some(part) = parts.next() {
        part
    } else {
        tracing::error!("Failed to read port: {line}");
        return Err(ParseSshConnectionError::Parse(line.into()));
    };

    // Followed by the server port.
    let _server_port = if let Some(part) = parts.next() {
        part
    } else {
        tracing::error!("Failed to read server_port: {line}");
        return Err(ParseSshConnectionError::Parse(line.into()));
    };

    // The last part should be our anchor.
    match parts.next() {
        Some("++\n") => {}
        None | Some(_) => {
            return Err(ParseSshConnectionError::Parse(line.into()));
        }
    };

    // Finally, there should be nothing left.
    if let Some(_) = parts.next() {
        tracing::error!("Extra data: {line}");
        return Err(ParseSshConnectionError::Parse(line.into()));
    }

    Ok((client_address.to_string(), None))
}

#[tracing::instrument(skip(stdout))]
async fn parse_ssh_connection<R: AsyncBufRead + Unpin>(
    stdout: &mut R,
    verbose: bool,
    ctx: &EnvironmentContext,
) -> std::result::Result<(String, Option<CompatibilityInfo>), ParseSshConnectionError> {
    let line = read_ssh_line_with_timeouts(stdout).await?;
    if line.is_empty() {
        tracing::error!("Failed to read first line from stdout");
        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
    }
    if verbose {
        write_ssh_log("O", &line, ctx).await;
    }
    if line.starts_with("{") {
        parse_ssh_connection_with_info(&line)
    } else {
        parse_ssh_connection_legacy(&line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAddr(pub String);

impl fmt::Display for HostAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<&str> for HostAddr {
    fn from(s: &str) -> Self {
        HostAddr(s.to_string())
    }
}

impl From<String> for HostAddr {
    fn from(s: String) -> Self {
        HostAddr(s)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum PipeError {
    #[error("compatibility check not supported")]
    NoCompatibilityCheck,
    #[error("could not establish connection: {0}")]
    ConnectionFailed(String),
    #[error("io error: {0}")]
    IoError(#[from] io::Error),
    #[error("error {0}")]
    Error(String),
    #[error("target referenced has gone")]
    TargetGone,
    #[error("creating pipe to {1} failed: {0}")]
    PipeCreationFailed(String, String),
    #[error("no ssh address to {0}")]
    NoAddress(String),
    #[error("running target overnet pipe: {0}")]
    SpawnError(String),
}

pub async fn parse_ssh_output(
    stdout: &mut BufReader<ChildStdout>,
    stderr: &mut BufReader<ChildStderr>,
    verbose_ssh: bool,
    ctx: &EnvironmentContext,
) -> std::result::Result<(HostAddr, Option<CompatibilityInfo>), PipeError> {
    let res = match parse_ssh_connection(stdout, verbose_ssh, ctx)
        .await
        .context("reading ssh connection")
    {
        Ok((addr, compatibility_status)) => (Some(HostAddr(addr)), compatibility_status),
        Err(e) => {
            let error_message = format!("Failed to read ssh client address: {e:?}");
            tracing::error!("{error_message}");
            (None, None)
        }
    };
    // Check for early exit.
    if let (Some(addr), compat) = res {
        Ok((addr, compat))
    } else {
        // If we failed to parse the ssh connection, there might be information in stderr
        Err(parse_ssh_error(stderr, verbose_ssh, ctx).await)
    }
}

#[tracing::instrument(skip(stderr))]
async fn parse_ssh_error<R: AsyncBufRead + Unpin>(
    stderr: &mut R,
    verbose: bool,
    ctx: &EnvironmentContext,
) -> PipeError {
    loop {
        let l = match read_ssh_line_with_timeouts(stderr).await {
            Err(e) => {
                tracing::error!("reading ssh stderr: {e:?}");
                return PipeError::NoCompatibilityCheck;
            }
            Ok(l) => l,
        };
        // Sadly, this is just reading buffered data, so timestamps in the log will be
        // incorrect
        if verbose {
            write_ssh_log("E", &l, ctx).await;
        }
        // If we are running with "ssh -v", the stderr will also contain the initial
        // "OpenSSH" line.
        if l.contains("OpenSSH") {
            continue;
        }
        // It also may contain a warning about adding an address to the list of known hosts
        if l.starts_with("Warning: Permanently added") {
            continue;
        }
        // Or a warning about authentication
        if l.starts_with("Authenticated to ") {
            continue;
        }
        // Additional debugging messages will begin with "debug{1,2,3}" based on the various
        // verbosity settings.
        if l.starts_with("debug1:") || l.starts_with("debug2:") || l.starts_with("debug3:") {
            continue;
        }
        // At this point, we just want to look at one line to see if it is the compatibility
        // failure.
        tracing::debug!("Reading stderr:  {l}");
        return if l.contains("Unrecognized argument: --abi-revision") {
            // It is an older image, so use the legacy command.
            tracing::info!(
                "Target does not support abi compatibility check, reverting to legacy connection"
            );
            PipeError::NoCompatibilityCheck
        } else {
            PipeError::ConnectionFailed(format!("{:?}", l))
        };
    }
}

fn parse_ssh_connection_with_info(
    line: &str,
) -> std::result::Result<(String, Option<CompatibilityInfo>), ParseSshConnectionError> {
    let connection_info: ConnectionInfo =
        serde_json::from_str(&line).map_err(|e| ParseSshConnectionError::Parse(e.to_string()))?;
    let mut parts = connection_info.ssh_connection.split(" ");
    // SSH_CONNECTION identifies the client and server ends of the connection.
    // The variable contains four space-separated values: client IP address,
    // client port number, server IP address, and server port number.
    if let Some(client_address) = parts.nth(0) {
        Ok((client_address.to_string(), Some(connection_info.compatibility)))
    } else {
        Err(ParseSshConnectionError::Parse(line.into()))
    }
}

pub async fn write_ssh_log(prefix: &str, line: &String, ctx: &EnvironmentContext) {
    // Skip keepalives, which will show up in the steady-state
    if line.contains("keepalive") {
        return;
    }
    let mut f = match ffx_config::logging::log_file_with_info(
        &ctx,
        &PathBuf::from("ssh.log"),
        LogDirHandling::WithDirWithRotate,
    ) {
        Ok((f, _)) => f,
        Err(e) => {
            tracing::warn!("Couldn't open ssh log file: {e:?}");
            return;
        }
    };
    const TIME_FORMAT: &str = "%b %d %H:%M:%S%.3f";
    let timestamp = chrono::Local::now().format(TIME_FORMAT);
    write!(&mut f, "{timestamp}: {prefix} {line}")
        .unwrap_or_else(|e| tracing::warn!("Couldn't write ssh log: {e:?}"));
}

#[cfg(test)]
mod test {
    use super::*;
    use assert_matches::assert_matches;

    #[fuchsia::test]
    async fn test_parse_ssh_output_doesnt_fail_with_debug2() {
        let env = ffx_config::test_init().await.expect("test env init");
        let line = "debug1: we are debugging\ndebug2: more debugging\ndebug1: debug one again\ndebug3: man this sure is verbose";
        let err = parse_ssh_error(&mut line.as_bytes(), true, &env.context).await;
        // Can't assert_eq because IoError doesn't impl PartialEq.
        assert!(matches!(err, PipeError::NoCompatibilityCheck));
    }

    #[fuchsia::test]
    async fn test_parse_ssh_connection_works() {
        let env = ffx_config::test_init().await.expect("test env init");
        for (line, expected) in [
            (&"++ 192.168.1.1 1234 10.0.0.1 22 ++\n"[..], ("192.168.1.1".to_string(), None)),
            (
                &"++ fe80::111:2222:3333:444 56671 10.0.0.1 22 ++\n",
                ("fe80::111:2222:3333:444".to_string(), None),
            ),
            (
                &"++ fe80::111:2222:3333:444%ethxc2 56671 10.0.0.1 22 ++\n",
                ("fe80::111:2222:3333:444%ethxc2".to_string(), None),
            ),
        ] {
            match parse_ssh_connection(&mut line.as_bytes(), false, &env.context).await {
                Ok(actual) => assert_eq!(expected, actual),
                res => panic!(
                    "unexpected result for {:?}: expected {:?}, got {:?}",
                    line, expected, res
                ),
            }
        }
    }

    #[fuchsia::test]
    async fn test_parse_ssh_connection_errors() {
        let env = ffx_config::test_init().await.expect("test env init");
        for line in [
            // Test for invalid anchors
            &"192.168.1.1 1234 10.0.0.1 22\n"[..],
            &"++192.168.1.1 1234 10.0.0.1 22++\n"[..],
            &"++192.168.1.1 1234 10.0.0.1 22 ++\n"[..],
            &"++ 192.168.1.1 1234 10.0.0.1 22++\n"[..],
            &"++ ++\n"[..],
            &"## 192.168.1.1 1234 10.0.0.1 22 ##\n"[..],
        ] {
            let res = parse_ssh_connection(&mut line.as_bytes(), false, &env.context).await;
            assert_matches!(res, Err(ParseSshConnectionError::Parse(_)));
        }
        for line in [
            // Truncation
            &"++"[..],
            &"++ 192.168.1.1"[..],
            &"++ 192.168.1.1 1234"[..],
            &"++ 192.168.1.1 1234 "[..],
            &"++ 192.168.1.1 1234 10.0.0.1"[..],
            &"++ 192.168.1.1 1234 10.0.0.1 22"[..],
            &"++ 192.168.1.1 1234 10.0.0.1 22 "[..],
            &"++ 192.168.1.1 1234 10.0.0.1 22 ++"[..],
        ] {
            let res = parse_ssh_connection(&mut line.as_bytes(), false, &env.context).await;
            assert_matches!(res, Err(ParseSshConnectionError::UnexpectedEOF(_)));
        }
    }

    #[fuchsia::test]
    async fn test_read_ssh_line() {
        let mut lb = LineBuffer::new();
        let input = &"++ 192.168.1.1 1234 10.0.0.1 22 ++\n"[..];
        match read_ssh_line(&mut lb, &mut input.as_bytes()).await {
            Ok(s) => assert_eq!(s, String::from("++ 192.168.1.1 1234 10.0.0.1 22 ++\n")),
            res => panic!("unexpected result: {res:?}"),
        }

        let mut lb = LineBuffer::new();
        let input = &"no newline"[..];
        let res = read_ssh_line(&mut lb, &mut input.as_bytes()).await;
        assert_matches!(res, Err(ParseSshConnectionError::UnexpectedEOF(_)));

        let mut lb = LineBuffer::new();
        let input = [b'A'; 1024];
        let res = read_ssh_line(&mut lb, &mut &input[..]).await;
        assert_matches!(res, Err(ParseSshConnectionError::Parse(_)));

        // Can continue after reading partial result
        let mut lb = LineBuffer::new();
        let input1 = &"foo"[..];
        let _ = read_ssh_line(&mut lb, &mut input1.as_bytes()).await;
        // We'll get a no-newline error, but it has the same semantics as
        // being interrupted due to a timeout
        let input2 = &"bar\n"[..];
        match read_ssh_line(&mut lb, &mut input2.as_bytes()).await {
            Ok(s) => assert_eq!(s, String::from("foobar\n")),
            res => panic!("unexpected result: {res:?}"),
        }
    }
}
