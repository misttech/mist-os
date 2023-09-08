// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{target::Target, RETRY_DELAY};
use anyhow::{anyhow, Context, Result};
use async_io::Async;
use async_trait::async_trait;
use ffx_daemon_core::events;
use ffx_daemon_events::{HostPipeErr, TargetEvent};
use ffx_ssh::ssh::build_ssh_command_with_ssh_path;
use fuchsia_async::{unblock, Task, TimeoutExt, Timer};
use futures::io::{copy_buf, AsyncBufRead, BufReader};
use futures::AsyncReadExt;
use futures_lite::{io::AsyncBufReadExt, stream::StreamExt};
use shared_child::SharedChild;
use std::{
    cell::RefCell,
    collections::VecDeque,
    fmt, io,
    net::SocketAddr,
    process::Stdio,
    rc::{Rc, Weak},
    sync::Arc,
    time::Duration,
};

const BUFFER_SIZE: usize = 65536;

#[derive(Debug)]
pub struct LogBuffer {
    buf: RefCell<VecDeque<String>>,
    capacity: usize,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self { buf: RefCell::new(VecDeque::with_capacity(capacity)), capacity }
    }

    pub fn push_line(&self, line: String) {
        let mut buf = self.buf.borrow_mut();
        if buf.len() == self.capacity {
            buf.pop_front();
        }

        buf.push_back(line)
    }

    pub fn lines(&self) -> Vec<String> {
        let buf = self.buf.borrow_mut();
        buf.range(..).cloned().collect()
    }

    pub fn clear(&self) {
        let mut buf = self.buf.borrow_mut();
        buf.truncate(0);
    }
}

#[derive(Debug, Clone)]
pub struct HostAddr(String);

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

#[async_trait(?Send)]
pub(crate) trait HostPipeChildBuilder {
    async fn new(
        &self,
        addr: SocketAddr,
        id: u64,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
    ) -> Result<(Option<HostAddr>, HostPipeChild)>
    where
        Self: Sized;

    fn ssh_path(&self) -> &str {
        "ssh"
    }
}

#[derive(Copy, Clone)]
pub(crate) struct HostPipeChildDefaultBuilder {}

#[async_trait(?Send)]
impl HostPipeChildBuilder for HostPipeChildDefaultBuilder {
    async fn new(
        &self,
        addr: SocketAddr,
        id: u64,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
    ) -> Result<(Option<HostAddr>, HostPipeChild)> {
        HostPipeChild::new_inner(
            self.ssh_path(),
            addr,
            id,
            stderr_buf,
            event_queue,
            watchdogs,
            ssh_timeout,
        )
        .await
    }
}

#[derive(Debug)]
pub(crate) struct HostPipeChild {
    inner: SharedChild,
    task: Option<Task<()>>,
}

fn setup_watchdogs() {
    use std::sync::atomic::{AtomicBool, Ordering};

    tracing::debug!("Setting up executor watchdog");
    let flag = Arc::new(AtomicBool::new(false));

    fuchsia_async::Task::spawn({
        let flag = Arc::clone(&flag);
        async move {
            fuchsia_async::Timer::new(std::time::Duration::from_secs(1)).await;
            flag.store(true, Ordering::Relaxed);
            tracing::debug!("Executor watchdog fired");
        }
    })
    .detach();

    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if !flag.load(Ordering::Relaxed) {
            tracing::error!("Aborting due to watchdog timeout!");
            std::process::abort();
        }
    });
}

impl HostPipeChild {
    #[tracing::instrument(skip(stderr_buf, event_queue))]
    async fn new_inner(
        ssh_path: &str,
        addr: SocketAddr,
        id: u64,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
    ) -> Result<(Option<HostAddr>, HostPipeChild)> {
        // TODO (119900): Re-enable ABI checks when ffx repository server uses ssh to setup
        // repositories for older devices.
        let id_string = format!("{}", id);
        let mut args = vec!["echo", "++ $SSH_CONNECTION ++", "&&", "remote_control_runner"];

        args.push("--circuit");
        args.push(id_string.as_str());

        // Before running remote_control_runner, we look up the environment
        // variable for $SSH_CONNECTION. This contains the IP address, including
        // scope_id, of the ssh client from the perspective of the ssh server.
        // This is useful because the target might need to use a specific
        // interface to talk to the host device.
        let mut ssh = build_ssh_command_with_ssh_path(ssh_path, addr, args).await?;

        tracing::debug!("Spawning new ssh instance: {:?}", ssh);

        if watchdogs {
            setup_watchdogs();
        }

        let mut ssh_cmd = ssh.stdout(Stdio::piped()).stdin(Stdio::piped()).stderr(Stdio::piped());

        let ssh = SharedChild::spawn(&mut ssh_cmd).context("running target overnet pipe")?;

        // todo(fxb/108692) remove this use of the global hoist when we put the main one in the environment context
        // instead -- this one is very deeply embedded, but isn't used by tests (that I've found).
        let (pipe_rx, mut pipe_tx) = futures::AsyncReadExt::split(
            overnet_pipe(hoist::hoist()).context("creating local overnet pipe")?,
        );

        let stdout =
            Async::new(ssh.take_stdout().ok_or(anyhow!("unable to get stdout from target pipe"))?)?;

        let mut stdin =
            Async::new(ssh.take_stdin().ok_or(anyhow!("unable to get stdin from target pipe"))?)?;

        let stderr =
            Async::new(ssh.take_stderr().ok_or(anyhow!("unable to stderr from target pipe"))?)?;

        // Read the first line. This can be either either be an empty string "",
        // which signifies the STDOUT has been closed, or the $SSH_CONNECTION
        // value.
        let mut stdout = BufReader::with_capacity(BUFFER_SIZE, stdout);

        tracing::debug!("Awaiting client address from ssh connection");
        let ssh_host_address = match parse_ssh_connection(&mut stdout, Some(ssh_timeout))
            .await
            .context("reading ssh connection")
        {
            Ok(addr) => Some(HostAddr(addr)),
            Err(e) => {
                tracing::error!("Failed to read ssh client address: {:?}", e);
                None
            }
        };
        tracing::debug!("Got ssh host address {ssh_host_address:?}");

        let copy_in = async move {
            if let Err(e) = copy_buf(stdout, &mut pipe_tx).await {
                tracing::error!("SSH stdout read failure: {:?}", e);
            }
        };
        let copy_out = async move {
            if let Err(e) =
                copy_buf(BufReader::with_capacity(BUFFER_SIZE, pipe_rx), &mut stdin).await
            {
                tracing::error!("SSH stdin write failure: {:?}", e);
            }
        };

        let log_stderr = async move {
            let mut stderr_lines = futures_lite::io::BufReader::new(stderr).lines();
            while let Some(result) = stderr_lines.next().await {
                match result {
                    Ok(line) => {
                        tracing::info!("SSH stderr: {}", line);
                        stderr_buf.push_line(line.clone());
                        event_queue
                            .push(TargetEvent::SshHostPipeErr(HostPipeErr::from(line)))
                            .unwrap_or_else(|e| {
                                tracing::warn!("queueing host pipe err event: {:?}", e)
                            });
                    }
                    Err(e) => tracing::error!("SSH stderr read failure: {:?}", e),
                }
            }
        };

        tracing::debug!("Establishing host-pipe process to target");
        Ok((
            ssh_host_address,
            HostPipeChild {
                inner: ssh,
                task: Some(Task::local(async move {
                    futures::join!(copy_in, copy_out, log_stderr);
                })),
            },
        ))
    }

    fn kill(&self) -> io::Result<()> {
        self.inner.kill()
    }

    fn wait(&self) -> io::Result<std::process::ExitStatus> {
        self.inner.wait()
    }
}

#[derive(Debug, thiserror::Error)]
enum ParseSshConnectionError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Parse error: {:?}", .0)]
    Parse(String),
    #[error("Read-line timeout")]
    Timeout,
}

const BUFSIZE: usize = 1024;
struct LineBuffer {
    buffer: [u8; BUFSIZE],
    pos: usize,
}

// 1K should be enough for the initial line, which just looks something like
//    "++ 192.168.1.1 1234 10.0.0.1 22 ++\n"
impl LineBuffer {
    fn new() -> Self {
        Self { buffer: [0; BUFSIZE], pos: 0 }
    }
    fn line(&self) -> &[u8] {
        &self.buffer[..self.pos]
    }
}

impl ToString for LineBuffer {
    fn to_string(&self) -> String {
        String::from_utf8_lossy(self.line()).into()
    }
}

#[tracing::instrument(skip(lb, rdr))]
async fn read_ssh_line<R: AsyncBufRead + Unpin>(
    lb: &mut LineBuffer,
    rdr: &mut R,
) -> std::result::Result<String, ParseSshConnectionError> {
    loop {
        // rdr is buffered, so it's not terrible to read a byte at a time, for just one line
        let mut b = [0u8];
        let n = rdr.read(&mut b[..]).await.map_err(ParseSshConnectionError::Io)?;
        let b = b[0];
        if n == 0 {
            return Err(ParseSshConnectionError::Parse(format!("No newline: {:?}", lb.line())));
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
            return Ok(lb.to_string());
        }
    }
}

#[tracing::instrument(skip(rdr))]
async fn read_ssh_line_with_timeouts<R: AsyncBufRead + Unpin>(
    rdr: &mut R,
    timeout: Option<u16>,
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
                tracing::debug!("No line after {time}, line so far: {:?}", lb.line());
                time += wait_time;
                if let Some(t) = timeout {
                    if time > t.into() {
                        return Err(ParseSshConnectionError::Timeout);
                    }
                }
            }
            Err(e) => {
                return Err(e);
            }
        }
    }
}

#[tracing::instrument(skip(rdr))]
async fn parse_ssh_connection<R: AsyncBufRead + Unpin>(
    rdr: &mut R,
    timeout: Option<u16>,
) -> std::result::Result<String, ParseSshConnectionError> {
    let line = read_ssh_line_with_timeouts(rdr, timeout).await?;
    if line.is_empty() {
        tracing::error!("Failed to first line");
        return Err(ParseSshConnectionError::Parse(line));
    }

    let mut parts = line.split(' ');

    // The first part should be our anchor.
    match parts.next() {
        Some("++") => {}
        Some(_) | None => {
            tracing::error!("Failed to read first anchor: {line}");
            return Err(ParseSshConnectionError::Parse(line));
        }
    }

    // The next part should be the client address. This is left as a string since
    // std::net::IpAddr does not support string scope_ids.
    let client_address = if let Some(part) = parts.next() {
        part
    } else {
        tracing::error!("Failed to read client_address: {line}");
        return Err(ParseSshConnectionError::Parse(line));
    };

    // Followed by the client port.
    let _client_port = if let Some(part) = parts.next() {
        part
    } else {
        tracing::error!("Failed to read port: {line}");
        return Err(ParseSshConnectionError::Parse(line));
    };

    // Followed by the server address.
    let _server_address = if let Some(part) = parts.next() {
        part
    } else {
        tracing::error!("Failed to read port: {line}");
        return Err(ParseSshConnectionError::Parse(line));
    };

    // Followed by the server port.
    let _server_port = if let Some(part) = parts.next() {
        part
    } else {
        tracing::error!("Failed to read server_port: {line}");
        return Err(ParseSshConnectionError::Parse(line));
    };

    // The last part should be our anchor.
    match parts.next() {
        Some("++\n") => {}
        Some(_) | None => {
            tracing::error!("Failed to read second anchor: {line}");
            return Err(ParseSshConnectionError::Parse(line));
        }
    }

    // Finally, there should be nothing left.
    if let Some(_) = parts.next() {
        tracing::error!("Extra data: {line}");
        return Err(ParseSshConnectionError::Parse(line));
    }

    Ok(client_address.to_string())
}

impl Drop for HostPipeChild {
    fn drop(&mut self) {
        match self.inner.try_wait() {
            Ok(Some(result)) => {
                tracing::info!("HostPipeChild exited with {}", result);
            }
            Ok(None) => {
                let _ = self
                    .kill()
                    .map_err(|e| tracing::warn!("failed to kill HostPipeChild: {:?}", e));
                let _ = self
                    .wait()
                    .map_err(|e| tracing::warn!("failed to clean up HostPipeChild: {:?}", e));
            }
            Err(e) => {
                tracing::warn!("failed to soft-wait HostPipeChild: {:?}", e);
                // defensive kill & wait, both may fail.
                let _ = self
                    .kill()
                    .map_err(|e| tracing::warn!("failed to kill HostPipeChild: {:?}", e));
                let _ = self
                    .wait()
                    .map_err(|e| tracing::warn!("failed to clean up HostPipeChild: {:?}", e));
            }
        };

        drop(self.task.take());
    }
}

#[derive(Debug)]
pub(crate) struct HostPipeConnection<T>
where
    T: HostPipeChildBuilder + Copy,
{
    target: Rc<Target>,
    inner: Arc<HostPipeChild>,
    relaunch_command_delay: Duration,
    host_pipe_child_builder: T,
    ssh_timeout: u16,
    watchdogs: bool,
}

impl<T> Drop for HostPipeConnection<T>
where
    T: HostPipeChildBuilder + Copy,
{
    fn drop(&mut self) {
        let res = self.inner.kill();
        tracing::info!("killed inner {:?}", res)
    }
}

pub(crate) async fn spawn(
    target: Weak<Target>,
    watchdogs: bool,
    ssh_timeout: u16,
) -> Result<HostPipeConnection<HostPipeChildDefaultBuilder>> {
    let host_pipe_child_builder = HostPipeChildDefaultBuilder {};
    HostPipeConnection::<HostPipeChildDefaultBuilder>::spawn_with_builder(
        target,
        host_pipe_child_builder,
        ssh_timeout,
        RETRY_DELAY,
        watchdogs,
    )
    .await
}

impl<T> HostPipeConnection<T>
where
    T: HostPipeChildBuilder + Copy,
{
    async fn start_child_pipe(
        target: &Weak<Target>,
        builder: T,
        ssh_timeout: u16,
        watchdogs: bool,
    ) -> Result<Arc<HostPipeChild>> {
        let target = target.upgrade().ok_or(anyhow!("Target has gone"))?;
        let target_nodename = target.nodename();
        tracing::debug!("Spawning new host-pipe instance to target {:?}", target_nodename);
        let log_buf = target.host_pipe_log_buffer();
        log_buf.clear();

        let ssh_address = target.ssh_address().ok_or_else(|| {
            anyhow!("target {:?} does not yet have an ssh address", target_nodename)
        })?;

        let (host_addr, cmd) = builder
            .new(
                ssh_address,
                target.id(),
                log_buf.clone(),
                target.events.clone(),
                watchdogs,
                ssh_timeout,
            )
            .await
            .with_context(|| {
                format!("creating host-pipe command to target {:?}", target_nodename)
            })?;

        *target.ssh_host_address.borrow_mut() = host_addr;
        tracing::debug!(
            "Set ssh_host_address to {:?} for {}@{}",
            target.ssh_host_address,
            target.nodename_str(),
            target.id()
        );
        let hpc = Arc::new(cmd);
        Ok(hpc)
    }

    async fn spawn_with_builder(
        target: Weak<Target>,
        host_pipe_child_builder: T,
        ssh_timeout: u16,
        relaunch_command_delay: Duration,
        watchdogs: bool,
    ) -> Result<Self> {
        let hpc = Self::start_child_pipe(&target, host_pipe_child_builder, ssh_timeout, watchdogs)
            .await?;
        let target = target.upgrade().ok_or(anyhow!("Target has gone"))?;

        Ok(Self {
            target,
            inner: hpc,
            relaunch_command_delay,
            host_pipe_child_builder,
            ssh_timeout,
            watchdogs,
        })
    }

    pub async fn wait(&mut self) -> Result<()> {
        loop {
            // Waits on the running the command. If it exits successfully (disconnect
            // due to peer dropping) then will set the target to disconnected
            // state. If there was an error running the command for some reason,
            // then continue and attempt to run the command again.
            let target_nodename = self.target.nodename();
            let clone = self.inner.clone();
            let res = unblock(move || clone.wait()).await.map_err(|e| {
                anyhow!(
                    "host-pipe error to target {:?} running try-wait: {}",
                    target_nodename,
                    e.to_string()
                )
            });

            tracing::debug!("host-pipe command res: {:?}", res);

            // Keep the ssh_host address in the target. This is the address of the host as seen from
            // the target. It is primarily used when configuring the package server address.
            tracing::debug!(
                "Skipped clearing ssh_host_address for {}@{}",
                self.target.nodename_str(),
                self.target.id()
            );

            match res {
                Ok(_) => {
                    return Ok(());
                }
                Err(e) => tracing::debug!("running cmd on {:?}: {:#?}", target_nodename, e),
            }

            // TODO(fxbug.dev/52038): Want an exponential backoff that
            // is sync'd with an explicit "try to start this again
            // anyway" channel using a select! between the two of them.
            tracing::debug!(
                "waiting {} before restarting child_pipe",
                self.relaunch_command_delay.as_secs()
            );
            Timer::new(self.relaunch_command_delay).await;

            let hpc = Self::start_child_pipe(
                &Rc::downgrade(&self.target),
                self.host_pipe_child_builder,
                self.ssh_timeout,
                self.watchdogs,
            )
            .await?;
            self.inner = hpc;
        }
    }
}

fn overnet_pipe(overnet_instance: &hoist::Hoist) -> Result<fidl::AsyncSocket> {
    let (local_socket, remote_socket) = fidl::Socket::create_stream();
    let local_socket = fidl::AsyncSocket::from_socket(local_socket)?;
    overnet_instance.start_client_socket(remote_socket).detach();

    Ok(local_socket)
}

#[cfg(test)]
mod test {
    use super::*;
    use addr::TargetAddr;
    use assert_matches::assert_matches;
    use std::process::Command;
    use std::{rc::Rc, str::FromStr};

    const ERR_CTX: &'static str = "running fake host-pipe command for test";

    impl HostPipeChild {
        /// Implements some fake join handles that wait on a join command before
        /// closing. The reader and writer handles don't do anything other than
        /// spin until they receive a message to stop.
        pub fn fake_new(child: &mut Command) -> Self {
            let child = SharedChild::spawn(child).context(ERR_CTX).unwrap();
            Self { inner: child, task: Some(Task::local(async {})) }
        }
    }

    #[derive(Copy, Clone, Debug)]
    enum ChildOperationType {
        Normal,
        InternalFailure,
        SshFailure,
    }

    #[derive(Copy, Clone, Debug)]
    struct FakeHostPipeChildBuilder {
        operation_type: ChildOperationType,
    }

    #[async_trait(?Send)]
    impl HostPipeChildBuilder for FakeHostPipeChildBuilder {
        async fn new(
            &self,
            addr: SocketAddr,
            id: u64,
            stderr_buf: Rc<LogBuffer>,
            event_queue: events::Queue<TargetEvent>,
            _watchdogs: bool,
            _ssh_timeout: u16,
        ) -> Result<(Option<HostAddr>, HostPipeChild)> {
            match self.operation_type {
                ChildOperationType::Normal => {
                    start_child_normal_operation(addr, id, stderr_buf, event_queue).await
                }
                ChildOperationType::InternalFailure => {
                    start_child_internal_failure(addr, id, stderr_buf, event_queue).await
                }
                ChildOperationType::SshFailure => {
                    start_child_ssh_failure(addr, id, stderr_buf, event_queue).await
                }
            }
        }
    }

    async fn start_child_normal_operation(
        _addr: SocketAddr,
        _id: u64,
        _buf: Rc<LogBuffer>,
        _events: events::Queue<TargetEvent>,
    ) -> Result<(Option<HostAddr>, HostPipeChild)> {
        Ok((
            Some(HostAddr("127.0.0.1".to_string())),
            HostPipeChild::fake_new(
                std::process::Command::new("echo")
                    .arg("127.0.0.1 44315 192.168.1.1 22")
                    .stdout(Stdio::piped())
                    .stdin(Stdio::piped()),
            ),
        ))
    }

    async fn start_child_internal_failure(
        _addr: SocketAddr,
        _id: u64,
        _buf: Rc<LogBuffer>,
        _events: events::Queue<TargetEvent>,
    ) -> Result<(Option<HostAddr>, HostPipeChild)> {
        Err(anyhow!(ERR_CTX))
    }

    async fn start_child_ssh_failure(
        _addr: SocketAddr,
        _id: u64,
        _buf: Rc<LogBuffer>,
        events: events::Queue<TargetEvent>,
    ) -> Result<(Option<HostAddr>, HostPipeChild)> {
        events.push(TargetEvent::SshHostPipeErr(HostPipeErr::Unknown("foo".to_string()))).unwrap();
        Ok((
            Some(HostAddr("127.0.0.1".to_string())),
            HostPipeChild::fake_new(
                std::process::Command::new("echo")
                    .arg("127.0.0.1 44315 192.168.1.1 22")
                    .stdout(Stdio::piped())
                    .stdin(Stdio::piped()),
            ),
        ))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_host_pipe_start_and_stop_normal_operation() {
        let target = crate::target::Target::new_with_addrs(
            Some("flooooooooberdoober"),
            [TargetAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let res = HostPipeConnection::<FakeHostPipeChildBuilder>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder { operation_type: ChildOperationType::Normal },
            30,
            Duration::default(),
            false,
        )
        .await;
        assert_matches!(res, Ok(_));
        // Shouldn't panic when dropped.
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_host_pipe_start_and_stop_internal_failure() {
        // TODO(awdavies): Verify the error matches.
        let target = crate::target::Target::new_with_addrs(
            Some("flooooooooberdoober"),
            [TargetAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let res = HostPipeConnection::<FakeHostPipeChildBuilder>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder { operation_type: ChildOperationType::InternalFailure },
            30,
            Duration::default(),
            false,
        )
        .await;
        assert!(res.is_err());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_host_pipe_start_and_stop_ssh_failure() {
        let target = crate::target::Target::new_with_addrs(
            Some("flooooooooberdoober"),
            [TargetAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let events = target.events.clone();
        let task = Task::local(async move {
            events
                .wait_for(None, |e| {
                    assert_matches!(e, TargetEvent::SshHostPipeErr(_));
                    true
                })
                .await
                .unwrap();
        });
        // This is here to allow for the above task to get polled so that the `wait_for` can be
        // placed on at the appropriate time (before the failure occurs in the function below).
        futures_lite::future::yield_now().await;
        let res = HostPipeConnection::<FakeHostPipeChildBuilder>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder { operation_type: ChildOperationType::SshFailure },
            30,
            Duration::default(),
            false,
        )
        .await;
        assert_matches!(res, Ok(_));
        // If things are not setup correctly this will hang forever.
        task.await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_parse_ssh_connection_works() {
        for (line, expected) in [
            (&"++ 192.168.1.1 1234 10.0.0.1 22 ++\n"[..], "192.168.1.1".to_string()),
            (
                &"++ fe80::111:2222:3333:444 56671 10.0.0.1 22 ++\n",
                "fe80::111:2222:3333:444".to_string(),
            ),
            (
                &"++ fe80::111:2222:3333:444%ethxc2 56671 10.0.0.1 22 ++\n",
                "fe80::111:2222:3333:444%ethxc2".to_string(),
            ),
        ] {
            match parse_ssh_connection(&mut line.as_bytes(), None).await {
                Ok(actual) => assert_eq!(expected, actual),
                res => panic!(
                    "unexpected result for {:?}: expected {:?}, got {:?}",
                    line, expected, res
                ),
            }
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_parse_ssh_connection_errors() {
        for line in [
            // Test for invalid anchors
            &"192.168.1.1 1234 10.0.0.1 22\n"[..],
            &"++192.168.1.1 1234 10.0.0.1 22++\n"[..],
            &"++192.168.1.1 1234 10.0.0.1 22 ++\n"[..],
            &"++ 192.168.1.1 1234 10.0.0.1 22++\n"[..],
            &"++ ++\n"[..],
            &"## 192.168.1.1 1234 10.0.0.1 22 ##\n"[..],
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
            let res = parse_ssh_connection(&mut line.as_bytes(), None).await;
            assert_matches!(res, Err(ParseSshConnectionError::Parse(_)));
        }
    }

    #[test]
    fn test_log_buffer_empty() {
        let buf = LogBuffer::new(2);
        assert!(buf.lines().is_empty());
    }

    #[test]
    fn test_log_buffer() {
        let buf = LogBuffer::new(2);

        buf.push_line(String::from("1"));
        buf.push_line(String::from("2"));
        buf.push_line(String::from("3"));

        assert_eq!(buf.lines(), vec![String::from("2"), String::from("3")]);
    }

    #[test]
    fn test_clear_log_buffer() {
        let buf = LogBuffer::new(2);

        buf.push_line(String::from("1"));
        buf.push_line(String::from("2"));

        buf.clear();

        assert!(buf.lines().is_empty());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_read_ssh_line() {
        // async fn read_ssh_line<R: AsyncBufRead + Unpin>(
        //     lb: &mut LineBuffer,
        //     rdr: &mut R,
        // ) -> std::result::Result<bool, ParseSshConnectionError> {
        let mut lb = LineBuffer::new();
        let input = &"++ 192.168.1.1 1234 10.0.0.1 22 ++\n"[..];
        match read_ssh_line(&mut lb, &mut input.as_bytes()).await {
            Ok(s) => assert_eq!(s, String::from("++ 192.168.1.1 1234 10.0.0.1 22 ++\n")),
            res => panic!("unexpected result: {res:?}"),
        }

        let mut lb = LineBuffer::new();
        let input = &"no newline"[..];
        let res = read_ssh_line(&mut lb, &mut input.as_bytes()).await;
        assert_matches!(res, Err(ParseSshConnectionError::Parse(_)));

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
