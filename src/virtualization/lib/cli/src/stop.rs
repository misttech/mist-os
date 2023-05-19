// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::platform::PlatformServices,
    anyhow::{anyhow, Error},
    fidl::endpoints::{create_proxy, Proxy},
    fidl_fuchsia_virtualization::{GuestManagerProxy, GuestMarker, GuestProxy, GuestStatus},
    fuchsia_async::{self as fasync, TimeoutExt},
    fuchsia_zircon_status::Status,
    guest_cli_args as arguments,
    std::fmt,
};

#[derive(Default, serde::Serialize, serde::Deserialize, PartialEq, Debug)]
pub enum StopStatus {
    #[default]
    NotStopped,
    NotRunning,
    Forced,
    Graceful,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct StopResult {
    pub status: StopStatus,
    pub stop_time_nanos: i64,
}

impl fmt::Display for StopResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let time_to_str = |nanos: i64| -> String {
            let duration = std::time::Duration::from_nanos(nanos as u64);
            if duration.as_millis() > 1 {
                format!("{}ms", duration.as_millis())
            } else {
                format!("{}μs", duration.as_micros())
            }
        };

        match self.status {
            StopStatus::NotStopped => write!(f, "Failed to stop guest"),
            StopStatus::NotRunning => write!(f, "Nothing to do - the guest is not running"),
            StopStatus::Forced => {
                write!(f, "Guest forced to stop in {}", time_to_str(self.stop_time_nanos))
            }
            StopStatus::Graceful => {
                write!(f, "Guest finished stopping in {}", time_to_str(self.stop_time_nanos))
            }
        }
    }
}

enum ShutdownCommand {
    DebianShutdownCommand,
    ZirconShutdownCommand,
}

pub async fn handle_stop<P: PlatformServices>(
    services: &P,
    args: &arguments::stop_args::StopArgs,
) -> Result<StopResult, Error> {
    let manager = services.connect_to_manager(args.guest_type).await?;
    let status = manager.get_info().await?.guest_status.expect("guest status should always be set");
    if status != GuestStatus::Starting && status != GuestStatus::Running {
        return Ok(StopResult { status: StopStatus::NotRunning, ..StopResult::default() });
    }

    if args.force {
        force_stop_guest(args.guest_type, manager).await
    } else {
        graceful_stop_guest(services, args.guest_type, manager).await
    }
}

fn get_graceful_stop_command(guest_cmd: ShutdownCommand) -> Vec<u8> {
    let arg_string = match guest_cmd {
        ShutdownCommand::ZirconShutdownCommand => "dm shutdown\n".to_string(),
        ShutdownCommand::DebianShutdownCommand => "shutdown now\n".to_string(),
    };

    arg_string.into_bytes()
}

async fn send_stop_shell_command(
    guest_cmd: ShutdownCommand,
    guest_endpoint: GuestProxy,
) -> Result<(), Error> {
    // TODO(fxbug.dev/111128): Use a different console for sending the stop command.
    let socket = guest_endpoint
        .get_console()
        .await
        .map_err(|err| anyhow!("failed to get a get_console response: {}", err))?
        .map_err(|err| anyhow!("get_console failed with: {:?}", err))?;

    println!("Sending stop command to guest");
    let command = get_graceful_stop_command(guest_cmd);
    let bytes_written = socket
        .write(&command)
        .map_err(|err| anyhow!("failed to write command to socket: {}", err))?;
    if bytes_written != command.len() {
        return Err(anyhow!(
            "attempted to send command '{}', but only managed to write '{}'",
            std::str::from_utf8(&command).expect("failed to parse as utf-8"),
            std::str::from_utf8(&command[0..bytes_written]).expect("failed to parse as utf-8")
        ));
    }

    Ok(())
}

async fn send_stop_rpc<P: PlatformServices>(
    services: &P,
    guest: arguments::GuestType,
) -> Result<(), Error> {
    assert!(guest == arguments::GuestType::Termina);
    let linux_manager = services.connect_to_linux_manager().await?;
    linux_manager
        .graceful_shutdown()
        .map_err(|err| anyhow!("failed to send shutdown to termina manager: {}", err))
}

async fn graceful_stop_guest<P: PlatformServices>(
    services: &P,
    guest: arguments::GuestType,
    manager: GuestManagerProxy,
) -> Result<StopResult, Error> {
    let (guest_endpoint, guest_server_end) = create_proxy::<GuestMarker>()
        .map_err(|err| anyhow!("failed to create guest proxy: {}", err))?;
    manager
        .connect(guest_server_end)
        .await
        .map_err(|err| anyhow!("failed to get a connect response: {}", err))?
        .map_err(|err| anyhow!("connect failed with: {:?}", err))?;

    match guest {
        arguments::GuestType::Zircon => {
            send_stop_shell_command(ShutdownCommand::ZirconShutdownCommand, guest_endpoint.clone())
                .await
        }
        arguments::GuestType::Debian => {
            send_stop_shell_command(ShutdownCommand::DebianShutdownCommand, guest_endpoint.clone())
                .await
        }
        arguments::GuestType::Termina => send_stop_rpc(services, guest).await,
    }?;

    let start = fasync::Time::now();
    println!("Waiting for guest to stop");

    let unresponsive_help_delay = fasync::Time::now() + std::time::Duration::from_secs(10).into();
    let guest_closed =
        guest_endpoint.on_closed().on_timeout(unresponsive_help_delay, || Err(Status::TIMED_OUT));

    match guest_closed.await {
        Ok(_) => Ok(()),
        Err(Status::TIMED_OUT) => {
            println!("If the guest is unresponsive, you may force stop it by passing -f");
            guest_endpoint.on_closed().await.map(|_| ())
        }
        Err(err) => Err(err),
    }
    .map_err(|err| anyhow!("failed to wait on guest stop signal: {}", err))?;

    let stop_time_nanos = get_time_nanos(fasync::Time::now() - start);
    Ok(StopResult { status: StopStatus::Graceful, stop_time_nanos })
}

async fn force_stop_guest(
    guest: arguments::GuestType,
    manager: GuestManagerProxy,
) -> Result<StopResult, Error> {
    println!("Forcing {} to stop", guest.to_string());
    let start = fasync::Time::now();
    manager.force_shutdown().await?;

    let stop_time_nanos = get_time_nanos(fasync::Time::now() - start);
    Ok(StopResult { status: StopStatus::Forced, stop_time_nanos })
}

fn get_time_nanos(duration: fasync::Duration) -> i64 {
    #[cfg(target_os = "fuchsia")]
    let nanos = duration.into_nanos();

    #[cfg(not(target_os = "fuchsia"))]
    let nanos = duration.as_nanos().try_into().unwrap();

    nanos
}

#[cfg(test)]
mod test {
    use {
        super::*,
        crate::platform::FuchsiaPlatformServices,
        async_utils::PollExt,
        fidl::{endpoints::create_proxy_and_stream, Socket},
        fidl_fuchsia_virtualization::GuestManagerMarker,
        futures::TryStreamExt,
    };

    #[test]
    fn graceful_stop_waits_for_shutdown() {
        let mut executor = fasync::TestExecutor::new_with_fake_time();
        executor.set_fake_time(fuchsia_async::Time::now());

        let (manager_proxy, mut manager_stream) = create_proxy_and_stream::<GuestManagerMarker>()
            .expect("failed to create GuestManager request stream");

        let service = FuchsiaPlatformServices::new();
        let fut = graceful_stop_guest(&service, arguments::GuestType::Debian, manager_proxy);
        futures::pin_mut!(fut);

        assert!(executor.run_until_stalled(&mut fut).is_pending());

        let (guest_server_end, responder) = executor
            .run_until_stalled(&mut manager_stream.try_next())
            .expect("future should be ready")
            .unwrap()
            .unwrap()
            .into_connect()
            .expect("received unexpected request on stream");

        responder.send(Ok(())).expect("failed to send response");
        let mut guest_stream = guest_server_end.into_stream().unwrap();

        assert!(executor.run_until_stalled(&mut fut).is_pending());

        let responder = executor
            .run_until_stalled(&mut guest_stream.try_next())
            .expect("future should be ready")
            .unwrap()
            .unwrap()
            .into_get_console()
            .expect("received unexpected request on stream");

        let (client, device) = Socket::create_stream();
        responder.send(Ok(client)).expect("failed to send response");

        assert!(executor.run_until_stalled(&mut fut).is_pending());

        let expected_command = get_graceful_stop_command(ShutdownCommand::DebianShutdownCommand);
        let mut actual_command = vec![0u8; expected_command.len()];
        assert_eq!(device.read(actual_command.as_mut_slice()).unwrap(), expected_command.len());

        // One nano past the helpful message timeout.
        let duration = std::time::Duration::from_secs(10) + std::time::Duration::from_nanos(1);
        executor.set_fake_time(fasync::Time::after((duration).into()));

        // Waiting for CHANNEL_PEER_CLOSED timed out (printing the helpful message), but then
        // a new indefinite wait began as the channel is still not closed.
        assert!(executor.wake_expired_timers());
        assert!(executor.run_until_stalled(&mut fut).is_pending());

        // Send a CHANNEL_PEER_CLOSED to the guest proxy.
        drop(guest_stream);

        let result = executor.run_until_stalled(&mut fut).expect("future should be ready").unwrap();
        assert_eq!(result.status, StopStatus::Graceful);
        assert_eq!(result.stop_time_nanos, duration.as_nanos() as i64);
    }

    #[test]
    fn force_stop_guest_calls_stop_endpoint() {
        let mut executor = fasync::TestExecutor::new();
        let (proxy, mut stream) = create_proxy_and_stream::<GuestManagerMarker>()
            .expect("failed to create GuestManager request stream");

        let fut = force_stop_guest(arguments::GuestType::Debian, proxy);
        futures::pin_mut!(fut);

        assert!(executor.run_until_stalled(&mut fut).is_pending());

        let responder = executor
            .run_until_stalled(&mut stream.try_next())
            .expect("future should be ready")
            .unwrap()
            .unwrap()
            .into_force_shutdown()
            .expect("received unexpected request on stream");
        responder.send().expect("failed to send response");

        let result = executor.run_until_stalled(&mut fut).expect("future should be ready").unwrap();
        assert_eq!(result.status, StopStatus::Forced);
    }
}
