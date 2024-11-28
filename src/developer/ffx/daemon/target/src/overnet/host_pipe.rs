// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::target::Target;
use crate::RETRY_DELAY;
use anyhow::anyhow;
use async_trait::async_trait;
use compat_info::CompatibilityInfo;
use ffx_config::EnvironmentContext;
use ffx_daemon_core::events;
use ffx_daemon_events::TargetEvent;
use ffx_ssh::parse::{
    parse_ssh_output, read_ssh_line, write_ssh_log, HostAddr, ParseSshConnectionError, PipeError,
};
use ffx_ssh::ssh::{build_ssh_command_with_env, SshError};
use fuchsia_async::{unblock, Task, TimeoutExt, Timer};
use nix::errno::Errno;
use nix::sys::signal::kill;
use nix::sys::signal::Signal::SIGKILL;
use nix::sys::wait::waitpid;
use nix::unistd::Pid;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::process::Stdio;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{copy_buf, BufReader};
use tokio::process::Child;

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

#[async_trait(?Send)]
pub(crate) trait HostPipeChildBuilder {
    type NodeType: Clone;
    async fn new(
        &self,
        addr: SocketAddr,
        id: u64,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
        node: Self::NodeType,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError>
    where
        Self: Sized;

    fn ssh_path(&self) -> &str;
}

#[derive(Copy, Clone)]
pub(crate) struct HostPipeChildDefaultBuilder<'a> {
    pub(crate) ssh_path: &'a str,
}

#[async_trait(?Send)]
impl HostPipeChildBuilder for HostPipeChildDefaultBuilder<'_> {
    type NodeType = Arc<overnet_core::Router>;
    async fn new(
        &self,
        addr: SocketAddr,
        id: u64,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
        node: Arc<overnet_core::Router>,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        let ctx = ffx_config::global_env_context().expect("Global env context uninitialized");
        let verbose_ssh = ffx_config::logging::debugging_on(&ctx);

        HostPipeChild::new_inner(
            self.ssh_path(),
            addr,
            id,
            stderr_buf,
            event_queue,
            watchdogs,
            ssh_timeout,
            verbose_ssh,
            node,
            ctx,
        )
        .await
    }

    fn ssh_path(&self) -> &str {
        self.ssh_path
    }
}

#[derive(Debug)]
pub(crate) struct HostPipeChild {
    inner: Child,
    task: Option<Task<()>>,
    pub(crate) compatibility_status: Option<CompatibilityInfo>,
    address: SocketAddr,
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
    pub fn get_compatibility_status(&self) -> Option<CompatibilityInfo> {
        self.compatibility_status.clone()
    }

    #[tracing::instrument(skip(stderr_buf, event_queue))]
    async fn new_inner_legacy(
        ssh_path: &str,
        addr: SocketAddr,
        id: u64,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
        verbose_ssh: bool,
        node: Arc<overnet_core::Router>,
        ctx: EnvironmentContext,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        let id_string = format!("{}", id);
        let args = vec![
            "echo",
            "++ $SSH_CONNECTION ++",
            "&&",
            "remote_control_runner",
            "--circuit",
            &id_string,
        ];

        Self::start_ssh_connection(
            ssh_path,
            addr,
            args,
            stderr_buf,
            event_queue,
            watchdogs,
            ssh_timeout,
            verbose_ssh,
            node,
            ctx,
        )
        .await
    }

    #[tracing::instrument(skip(stderr_buf, event_queue))]
    async fn new_inner(
        ssh_path: &str,
        addr: SocketAddr,
        id: u64,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
        verbose_ssh: bool,
        node: Arc<overnet_core::Router>,
        ctx: EnvironmentContext,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        let id_string = format!("{}", id);

        // pass the abi revision as a base 10 number so it is easy to parse.
        let rev: u64 =
            version_history_data::HISTORY.get_misleading_version_for_ffx().abi_revision.as_u64();
        let abi_revision = format!("{}", rev);
        let args =
            vec!["remote_control_runner", "--circuit", &id_string, "--abi-revision", &abi_revision];

        match Self::start_ssh_connection(
            ssh_path,
            addr,
            args,
            stderr_buf.clone(),
            event_queue.clone(),
            watchdogs,
            ssh_timeout,
            verbose_ssh,
            Arc::clone(&node),
            ctx.clone(),
        )
        .await
        {
            Ok((addr, pipe)) => Ok((addr, pipe)),
            Err(PipeError::NoCompatibilityCheck) => {
                Self::new_inner_legacy(
                    ssh_path,
                    addr,
                    id,
                    stderr_buf,
                    event_queue,
                    watchdogs,
                    ssh_timeout,
                    verbose_ssh,
                    node,
                    ctx,
                )
                .await
            }
            Err(e) => Err(e),
        }
    }

    async fn start_ssh_connection(
        ssh_path: &str,
        addr: SocketAddr,
        mut args: Vec<&str>,
        stderr_buf: Rc<LogBuffer>,
        event_queue: events::Queue<TargetEvent>,
        watchdogs: bool,
        ssh_timeout: u16,
        verbose_ssh: bool,
        node: Arc<overnet_core::Router>,
        ctx: EnvironmentContext,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        if verbose_ssh {
            args.insert(0, "-vv");
        }

        let mut ssh = tokio::process::Command::from(
            build_ssh_command_with_env(ssh_path, addr, &ctx, args)
                .await
                .map_err(|e| PipeError::Error(e.to_string()))?,
        );

        tracing::debug!("Spawning new ssh instance: {:?}", ssh);

        if watchdogs {
            setup_watchdogs();
        }

        let ssh_cmd = ssh.stdout(Stdio::piped()).stdin(Stdio::piped()).stderr(Stdio::piped());

        let mut ssh = ssh_cmd.spawn().map_err(|e| PipeError::SpawnError(e.to_string()))?;

        let (pipe_rx, mut pipe_tx) =
            tokio::io::split(ffx_target::create_overnet_socket(node).map_err(|e| {
                PipeError::PipeCreationFailed(
                    format!("creating local overnet pipe: {e}"),
                    addr.to_string(),
                )
            })?);

        let stdout = ssh
            .stdout
            .take()
            .ok_or_else(|| PipeError::Error("unable to get stdout from target pipe".into()))?;

        let mut stdin = ssh
            .stdin
            .take()
            .ok_or_else(|| PipeError::Error("unable to get stdin from target pipe".into()))?;

        let stderr = ssh
            .stderr
            .take()
            .ok_or_else(|| PipeError::Error("unable to stderr from target pipe".into()))?;

        // Read the first line. This can be either either be an empty string "",
        // which signifies the STDOUT has been closed, or the $SSH_CONNECTION
        // value.
        let mut stdout = BufReader::with_capacity(BUFFER_SIZE, stdout);
        // Also read stderr to determine whether we are talking to an old remote_control_runner that
        // doesn't support the `--abi-revision` argument.
        let mut stderr = BufReader::with_capacity(BUFFER_SIZE, stderr);

        tracing::debug!("Awaiting client address from ssh connection");
        let ssh_timeout = Duration::from_secs(ssh_timeout as u64);
        let (ssh_host_address, compatibility_status) =
            match parse_ssh_output(&mut stdout, &mut stderr, verbose_ssh, &ctx)
                .on_timeout(ssh_timeout, || {
                    Err(PipeError::ConnectionFailed(format!(
                        "ssh connection timed out after {ssh_timeout:?}"
                    )))
                })
                .await
            {
                Ok(res) => res,
                Err(e) => {
                    ssh.kill().await?;
                    let ssh_err = ffx_ssh::ssh::SshError::from(e.to_string());
                    if let Some(status) = ssh.try_wait()? {
                        match status.code() {
                            // Possible to catch more error codes here, hence the use of a match.
                            Some(255) => {
                                tracing::warn!("SSH ret code: 255. Unexpected session termination.")
                            }
                            _ => tracing::error!("SSH exited with error code: {status}. "),
                        }
                    } else {
                        tracing::error!(
                            "ssh child has not ended, trying one more time then ignoring it."
                        );
                        fuchsia_async::Timer::new(std::time::Duration::from_secs(2)).await;
                        tracing::error!("ssh child status is {:?}", ssh.try_wait());
                    }
                    event_queue.push(TargetEvent::SshHostPipeErr(ssh_err)).unwrap_or_else(|e| {
                        tracing::warn!("queueing host pipe err event: {:?}", e)
                    });
                    return Err(e);
                }
            };

        let copy_in = async move {
            if let Err(e) = copy_buf(&mut stdout, &mut pipe_tx).await {
                tracing::error!("SSH stdout read failure: {:?}", e);
            }
        };
        let copy_out = async move {
            if let Err(e) =
                copy_buf(&mut BufReader::with_capacity(BUFFER_SIZE, pipe_rx), &mut stdin).await
            {
                tracing::error!("SSH stdin write failure: {:?}", e);
            }
        };

        let log_stderr = async move {
            let mut lb = ffx_ssh::parse::LineBuffer::new();
            loop {
                let result = read_ssh_line(&mut lb, &mut stderr).await;
                match result {
                    Ok(line) => {
                        // TODO(slgrady) -- either remove this once we stop having
                        // ssh connection problems; or change it so that once we
                        // know the connection is established, the error messages
                        // go to the event queue as normal.
                        if verbose_ssh {
                            write_ssh_log("E", &line, &ctx).await;
                        } else {
                            // Sometimes the SSH message that comes from openssh has a carriage
                            // return at the end which messes up the flow of the info log.
                            tracing::info!("SSH stderr: {:?}", line.trim());
                            stderr_buf.push_line(line.clone());
                            event_queue
                                .push(TargetEvent::SshHostPipeErr(SshError::from(line)))
                                .unwrap_or_else(|e| {
                                    tracing::warn!("queueing host pipe err event: {:?}", e)
                                });
                        }
                    }
                    Err(ParseSshConnectionError::UnexpectedEOF(s)) => {
                        if !s.is_empty() {
                            tracing::error!("Got unexpected EOF -- buffer so far: {s:?}");
                        }
                        break;
                    }
                    Err(e) => tracing::error!("SSH stderr read failure: {:?}", e),
                }
            }
        };

        tracing::debug!("Establishing host-pipe process to target");
        Ok((
            Some(ssh_host_address),
            HostPipeChild {
                inner: ssh,
                task: Some(Task::local(async move {
                    futures::join!(copy_in, copy_out, log_stderr);
                })),
                compatibility_status,
                address: addr,
            },
        ))
    }
}

impl Drop for HostPipeChild {
    fn drop(&mut self) {
        let pid = Pid::from_raw(self.inner.id().unwrap() as i32);
        match self.inner.try_wait() {
            Ok(Some(result)) => {
                tracing::info!("HostPipeChild exited with {}", result);
            }
            Ok(None) => {
                let _ = kill(pid, SIGKILL)
                    .map_err(|e| tracing::debug!("failed to kill HostPipeChild: {:?}", e));
                let _ = waitpid(pid, None)
                    .map_err(|e| tracing::debug!("failed to clean up HostPipeChild: {:?}", e));
            }
            Err(e) => {
                // Let the user know if error returned from try_wait() is ESRCH
                if e.kind() == io::Error::from(Errno::ESRCH).kind() {
                    tracing::warn!("Failed to wait. No process found with the given PID: {pid}");
                } else {
                    tracing::debug!("failed to soft-wait HostPipeChild: {:?}", e);
                    let _ = kill(pid, SIGKILL)
                        .map_err(|e| tracing::debug!("failed to kill HostPipeChild: {:?}", e));
                    let _ = waitpid(pid, None)
                        .map_err(|e| tracing::debug!("failed to clean up HostPipeChild: {:?}", e));
                }
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
        let pid = Pid::from_raw(self.inner.inner.id().unwrap() as i32);
        let res = kill(pid, SIGKILL);
        match res {
            Err(Errno::ESRCH) => {
                tracing::warn!("Failed to kill. No process found with the given PID: {pid}");
            }
            Err(e) => {
                tracing::debug!("Failed to kill. Got {e:?}");
            }
            _ => (),
        };
    }
}

pub(crate) async fn spawn<'a>(
    target: Weak<Target>,
    watchdogs: bool,
    ssh_timeout: u16,
    node: Arc<overnet_core::Router>,
) -> Result<HostPipeConnection<HostPipeChildDefaultBuilder<'a>>, anyhow::Error> {
    let host_pipe_child_builder = HostPipeChildDefaultBuilder { ssh_path: "ssh" };
    HostPipeConnection::<HostPipeChildDefaultBuilder<'_>>::spawn_with_builder(
        target,
        host_pipe_child_builder,
        ssh_timeout,
        RETRY_DELAY,
        watchdogs,
        node,
    )
    .await
    .map_err(|e| anyhow!(e))
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
        node: T::NodeType,
    ) -> Result<Arc<HostPipeChild>, PipeError> {
        let target = target.upgrade().ok_or(PipeError::TargetGone)?;
        let target_nodename: String = target.nodename_str();
        tracing::debug!("Spawning new host-pipe instance to target {target_nodename}");
        let log_buf = target.host_pipe_log_buffer();
        log_buf.clear();

        let ssh_address =
            target.ssh_address().ok_or_else(|| PipeError::NoAddress(target_nodename.clone()))?;

        let (host_addr, cmd) = builder
            .new(
                ssh_address,
                target.id(),
                log_buf.clone(),
                target.events.clone(),
                watchdogs,
                ssh_timeout,
                node,
            )
            .await
            .map_err(|e| PipeError::PipeCreationFailed(e.to_string(), target_nodename.clone()))?;

        *target.ssh_host_address.borrow_mut() = host_addr;
        tracing::debug!(
            "Set ssh_host_address to {:?} for {}@{}",
            target.ssh_host_address,
            target.nodename_str(),
            target.id(),
        );
        if cmd.compatibility_status.is_some() {
            target.set_compatibility_status(&cmd.compatibility_status);
        }
        let hpc = Arc::new(cmd);
        Ok(hpc)
    }

    async fn spawn_with_builder(
        target: Weak<Target>,
        host_pipe_child_builder: T,
        ssh_timeout: u16,
        relaunch_command_delay: Duration,
        watchdogs: bool,
        node: T::NodeType,
    ) -> Result<Self, PipeError> {
        let hpc =
            Self::start_child_pipe(&target, host_pipe_child_builder, ssh_timeout, watchdogs, node)
                .await?;
        let target = target.upgrade().ok_or(PipeError::TargetGone)?;

        Ok(Self {
            target,
            inner: hpc,
            relaunch_command_delay,
            host_pipe_child_builder,
            ssh_timeout,
            watchdogs,
        })
    }

    pub async fn wait(&mut self, node: &T::NodeType) -> Result<(), anyhow::Error> {
        loop {
            // Waits on the running the command. If it exits successfully (disconnect
            // due to peer dropping) then will set the target to disconnected
            // state. If there was an error running the command for some reason,
            // then continue and attempt to run the command again.
            let pid = Pid::from_raw(self.inner.inner.id().unwrap() as i32);
            let target_nodename = self.target.nodename();
            let res = unblock(move || waitpid(pid, None)).await;

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

            // TODO(https://fxbug.dev/42129296): Want an exponential backoff that
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
                node.clone(),
            )
            .await?;
            self.inner = hpc;
        }
    }

    pub fn get_compatibility_status(&self) -> Option<CompatibilityInfo> {
        self.inner.get_compatibility_status()
    }

    pub fn get_address(&self) -> SocketAddr {
        self.inner.address
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use addr::TargetAddr;
    use assert_matches::assert_matches;
    use ffx_config::ConfigLevel;
    use serde_json::json;
    use std::fs;
    use std::net::Ipv4Addr;
    use std::os::unix::prelude::PermissionsExt;
    use std::str::FromStr;
    use tokio::process::Command;

    const ERR_CTX: &'static str = "running fake host-pipe command for test";

    impl HostPipeChild {
        /// Implements some fake join handles that wait on a join command before
        /// closing. The reader and writer handles don't do anything other than
        /// spin until they receive a message to stop.
        pub fn fake_new(child: &mut Command) -> Self {
            Self {
                inner: child.spawn().unwrap(),
                task: Some(Task::local(async {})),
                compatibility_status: None,
                address: SocketAddr::new(Ipv4Addr::new(192, 0, 2, 0).into(), 2345),
            }
        }
    }

    #[derive(Copy, Clone, Debug)]
    enum ChildOperationType {
        Normal,
        InternalFailure,
        SshFailure,
        DefaultBuilder,
    }

    #[derive(Copy, Clone, Debug)]
    struct FakeHostPipeChildBuilder<'a> {
        operation_type: ChildOperationType,
        ssh_path: &'a str,
    }

    #[async_trait(?Send)]
    impl HostPipeChildBuilder for FakeHostPipeChildBuilder<'_> {
        type NodeType = ();
        async fn new(
            &self,
            addr: SocketAddr,
            id: u64,
            stderr_buf: Rc<LogBuffer>,
            event_queue: events::Queue<TargetEvent>,
            watchdogs: bool,
            ssh_timeout: u16,
            _node: (),
        ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
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
                ChildOperationType::DefaultBuilder => {
                    let builder = HostPipeChildDefaultBuilder { ssh_path: self.ssh_path };
                    builder
                        .new(
                            addr,
                            id,
                            stderr_buf,
                            event_queue,
                            watchdogs,
                            ssh_timeout,
                            overnet_core::Router::new(None).unwrap(),
                        )
                        .await
                }
            }
        }

        fn ssh_path(&self) -> &str {
            self.ssh_path
        }
    }

    async fn start_child_normal_operation(
        _addr: SocketAddr,
        _id: u64,
        _buf: Rc<LogBuffer>,
        _events: events::Queue<TargetEvent>,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        Ok((
            Some(HostAddr("127.0.0.1".to_string())),
            HostPipeChild::fake_new(
                tokio::process::Command::new("echo")
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
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        Err(PipeError::Error(ERR_CTX.into()))
    }

    async fn start_child_ssh_failure(
        _addr: SocketAddr,
        _id: u64,
        _buf: Rc<LogBuffer>,
        events: events::Queue<TargetEvent>,
    ) -> Result<(Option<HostAddr>, HostPipeChild), PipeError> {
        events.push(TargetEvent::SshHostPipeErr(SshError::Unknown("foo".to_string()))).unwrap();
        Ok((
            Some(HostAddr("127.0.0.1".to_string())),
            HostPipeChild::fake_new(
                tokio::process::Command::new("echo")
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
        let res = HostPipeConnection::<FakeHostPipeChildBuilder<'_>>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder {
                operation_type: ChildOperationType::Normal,
                ssh_path: "ssh",
            },
            30,
            Duration::default(),
            false,
            (),
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
        let res = HostPipeConnection::<FakeHostPipeChildBuilder<'_>>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder {
                operation_type: ChildOperationType::InternalFailure,
                ssh_path: "ssh",
            },
            30,
            Duration::default(),
            false,
            (),
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
        let res = HostPipeConnection::<FakeHostPipeChildBuilder<'_>>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder {
                operation_type: ChildOperationType::SshFailure,
                ssh_path: "ssh",
            },
            30,
            Duration::default(),
            false,
            (),
        )
        .await;
        assert_matches!(res, Ok(_));
        // If things are not setup correctly this will hang forever.
        task.await;
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

    async fn write_test_ssh_keys(env: &ffx_config::TestEnv) {
        // Set the ssh key paths to something, the contents do no matter for this test.
        env.context
            .query("ssh.pub")
            .level(Some(ConfigLevel::User))
            .set(json!([env.isolate_root.path().join("test_authorized_keys")]))
            .await
            .expect("setting ssh pub key");

        let ssh_priv = env.isolate_root.path().join("test_ed25519_key");
        fs::write(&ssh_priv, "test-key").expect("writing test key");
        env.context
            .query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!([ssh_priv.to_string_lossy()]))
            .await
            .expect("setting ssh priv key");
    }

    #[fuchsia::test]
    async fn test_start_with_failure() {
        let env = ffx_config::test_init().await.unwrap();
        write_test_ssh_keys(&env).await;

        let target = crate::target::Target::new_with_addrs(
            Some("test_target"),
            [TargetAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let _res = HostPipeConnection::<FakeHostPipeChildBuilder<'_>>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder {
                operation_type: ChildOperationType::DefaultBuilder,
                ssh_path: "echo",
            },
            30,
            Duration::default(),
            false,
            (),
        )
        .await
        .expect_err("host connection");
    }

    #[fuchsia::test]
    async fn test_start_ok() {
        let env = ffx_config::test_init().await.unwrap();
        const SUPPORTED_HOST_PIPE_SH: &str = include_str!("../../test_data/supported_host_pipe.sh");

        let ssh_path = env.isolate_root.path().join("supported_host_pipe.sh");
        fs::write(&ssh_path, SUPPORTED_HOST_PIPE_SH).expect("writing test script");
        fs::set_permissions(&ssh_path, fs::Permissions::from_mode(0o770))
            .expect("setting permissions");

        write_test_ssh_keys(&env).await;

        let target = crate::target::Target::new_with_addrs(
            Some("test_target"),
            [TargetAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let ssh_path_str: String = ssh_path.to_string_lossy().to_string();
        let _res = HostPipeConnection::<FakeHostPipeChildBuilder<'_>>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder {
                operation_type: ChildOperationType::DefaultBuilder,
                ssh_path: &ssh_path_str,
            },
            30,
            Duration::default(),
            false,
            (),
        )
        .await
        .expect("host connection");
    }

    #[fuchsia::test]
    async fn test_start_legacy_ok() {
        let env = ffx_config::test_init().await.unwrap();
        const SUPPORTED_HOST_PIPE_SH: &str = include_str!("../../test_data/legacy_host_pipe.sh");

        let ssh_path = env.isolate_root.path().join("legacy_host_pipe.sh");
        fs::write(&ssh_path, SUPPORTED_HOST_PIPE_SH).expect("writing test script");
        fs::set_permissions(&ssh_path, fs::Permissions::from_mode(0o770))
            .expect("setting permissions");

        write_test_ssh_keys(&env).await;

        let target = crate::target::Target::new_with_addrs(
            Some("test_target"),
            [TargetAddr::from_str("192.168.1.1:22").unwrap()].into(),
        );
        let ssh_path_str: String = ssh_path.to_string_lossy().to_string();
        let _res = HostPipeConnection::<FakeHostPipeChildBuilder<'_>>::spawn_with_builder(
            Rc::downgrade(&target),
            FakeHostPipeChildBuilder {
                operation_type: ChildOperationType::DefaultBuilder,
                ssh_path: &ssh_path_str,
            },
            30,
            Duration::default(),
            false,
            (),
        )
        .await
        .expect("host connection");
    }

    #[fuchsia::test]
    async fn test_ssh_command_includes_keepalive_timeout() {
        let env = ffx_config::test_init().await.unwrap();
        write_test_ssh_keys(&env).await;

        env.context
            .query(ffx_ssh::ssh::KEEPALIVE_TIMEOUT_CONFIG)
            .level(Some(ConfigLevel::User))
            .set(json!(30))
            .await
            .expect("setting keepalive timeout key");

        let addr = SocketAddr::new(Ipv4Addr::new(192, 0, 2, 0).into(), 2345);
        let cmd = tokio::process::Command::from(
            build_ssh_command_with_env("path-to-ssh", addr, &env.context, vec![]).await.unwrap(),
        );
        // Kind of a hack, but there's no non-debug method that returns a string corresponding to the command.
        assert!(format!("{cmd:?}").contains("ServerAliveCountMax=30"));
    }
}
