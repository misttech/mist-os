// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::platform::{GuestConsole, PlatformServices, Stdio},
    anyhow::{anyhow, Error},
    fidl_fuchsia_virtualization::{GuestConfig, GuestManagerError, GuestMarker, GuestProxy},
    fuchsia_async as fasync, guest_cli_args as arguments,
    std::fmt,
};

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LaunchResult {
    LaunchCompleted,
    AttachFailed(String),
    RoutingError(arguments::GuestType),
    FidlError(String),
    LaunchFailure(u32),
}

impl fmt::Display for LaunchResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchResult::LaunchCompleted => write!(f, "Successfully launched the guest"),
            LaunchResult::AttachFailed(error) => {
                write!(f, "Failed to attach to a running guest: {}", error)
            }
            LaunchResult::FidlError(error) => write!(f, "Failed FIDL call: {}", error),
            LaunchResult::LaunchFailure(err) => write!(
                f,
                "Failed to launch guest: {:?}",
                GuestManagerError::from_primitive(*err).expect("expected a valid error")
            ),
            LaunchResult::RoutingError(guest_type) => {
                writeln!(f, "")?;
                writeln!(f, "Unable to connect to start the guest.")?;
                writeln!(
                    f,
                    "  Ensure you have the guest and core shards available on in your build:"
                )?;
                writeln!(f, "      fx set ... \\")?;
                writeln!(f, "          --with-base {} \\", guest_type.gn_target_label())?;
                writeln!(
                    f,
                    "          --args='core_realm_shards += [ \"{}\" ]'",
                    guest_type.gn_core_shard_label()
                )?;
                writeln!(f, "")
            }
        }
    }
}

pub async fn handle_launch<P: PlatformServices>(
    services: &P,
    args: &arguments::launch_args::LaunchArgs,
) -> LaunchResult {
    let config = parse_vmm_args(args);
    let guest = launch(services, args.guest_type, config).await;
    if let Err(err) = guest {
        return err;
    }

    if !args.detach {
        if let Err(err) = attach(guest.unwrap()).await {
            return LaunchResult::AttachFailed(format!("{}", err));
        }
    }

    LaunchResult::LaunchCompleted
}

fn parse_vmm_args(arguments: &arguments::launch_args::LaunchArgs) -> GuestConfig {
    // FIDL requires we make a GuestConfig::EMPTY before trying to update fields
    let mut guest_config = GuestConfig::EMPTY;

    if !arguments.cmdline_add.is_empty() {
        guest_config.cmdline_add = Some(arguments.cmdline_add.clone())
    };

    guest_config.guest_memory = arguments.memory;
    guest_config.cpus = arguments.cpus;
    guest_config.default_net = arguments.default_net;
    guest_config.virtio_balloon = arguments.virtio_balloon;
    guest_config.virtio_console = arguments.virtio_console;
    guest_config.virtio_gpu = arguments.virtio_gpu;
    guest_config.virtio_rng = arguments.virtio_rng;
    guest_config.virtio_sound = arguments.virtio_sound;
    guest_config.virtio_sound_input = arguments.virtio_sound_input;
    guest_config.virtio_vsock = arguments.virtio_vsock;

    guest_config
}

// Connect to a guest manager and launch the corresponding guest.
async fn launch<P: PlatformServices>(
    services: &P,
    guest_type: arguments::GuestType,
    config: GuestConfig,
) -> Result<GuestProxy, LaunchResult> {
    let (guest, guest_server_end) = fidl::endpoints::create_proxy::<GuestMarker>()
        .map_err(|err| LaunchResult::FidlError(format!("Create proxy - {}", err)))?;

    println!("Starting {}", guest_type.to_string());
    let manager = services
        .connect_to_manager(guest_type)
        .await
        .map_err(|err| LaunchResult::FidlError(format!("Connect to manager - {}", err)))?;

    match manager.launch(config, guest_server_end).await {
        Err(fidl::Error::ClientChannelClosed { .. }) => Err(LaunchResult::RoutingError(guest_type)),
        Err(err) => Err(LaunchResult::FidlError(format!("Send launch message - {}", err))),
        Ok(launch_result) => match launch_result {
            Ok(()) => Ok(guest),
            Err(error) => Err(LaunchResult::LaunchFailure(error.into_primitive())),
        },
    }
}

// Attach to a running guest, using a combined stdout and serial for output, and stdin for input.
async fn attach(guest: GuestProxy) -> Result<(), Error> {
    // Tie serial output to stdout.
    let guest_serial_response = guest.get_serial().await?;
    let guest_serial = fasync::Socket::from_socket(guest_serial_response)?;
    let serial_output = async {
        futures::io::copy(guest_serial, &mut GuestConsole::get_unblocked_stdio(Stdio::Stdout))
            .await
            .map(|_| ())
            .map_err(anyhow::Error::from)
    };

    // Host doesn't currently support duplicating Fuchsia handles, so just call get console twice
    // and let the VMM duplicate the socket for reading and writing.
    let console_input = guest.get_console().await?.map_err(|err| anyhow!(format!("{:?}", err)))?;
    let console_output = guest.get_console().await?.map_err(|err| anyhow!(format!("{:?}", err)))?;
    let guest_console = GuestConsole::new(console_input, console_output)?;

    futures::future::try_join(serial_output, guest_console.run_with_stdio())
        .await
        .map(|_| ())
        .map_err(anyhow::Error::from)
}

#[cfg(test)]
mod test {
    use {
        super::*,
        fidl::{endpoints::create_proxy_and_stream, Socket, SocketOpts},
        fidl_fuchsia_virtualization::GuestError,
        futures::future::join,
        futures::StreamExt,
    };

    #[fuchsia_async::run_until_stalled(test)]
    async fn launch_invalid_console_returns_error() {
        let (guest_proxy, mut guest_stream) = create_proxy_and_stream::<GuestMarker>().unwrap();
        let (serial_launch_sock, _serial_server_sock) = Socket::create(SocketOpts::STREAM).unwrap();

        let server = async move {
            let serial_responder = guest_stream
                .next()
                .await
                .expect("Failed to read from stream")
                .expect("Failed to parse request")
                .into_get_serial()
                .expect("Unexpected call to Guest Proxy");
            serial_responder.send(serial_launch_sock).expect("Failed to send request to proxy");

            let console_responder = guest_stream
                .next()
                .await
                .expect("Failed to read from stream")
                .expect("Failed to parse request")
                .into_get_console()
                .expect("Unexpected call to Guest Proxy");
            console_responder
                .send(&mut Err(GuestError::DeviceNotPresent))
                .expect("Failed to send request to proxy");
        };

        let client = attach(guest_proxy);
        let (_, client_res) = join(server, client).await;
        assert!(client_res.is_err());
    }
}
