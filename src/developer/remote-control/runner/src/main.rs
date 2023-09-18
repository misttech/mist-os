// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context as _, Result},
    argh::FromArgs,
    compat_info::{CompatibilityInfo, CompatibilityState, ConnectionInfo},
    fidl_fuchsia_developer_remotecontrol::RemoteControlMarker,
    fuchsia_component::client::connect_to_protocol,
    futures::future::select,
    futures::io::BufReader,
    futures::prelude::*,
    std::os::unix::io::{AsRawFd, FromRawFd},
    version_history::{check_abi_revision, AbiRevision},
};

const BUFFER_SIZE: usize = 65536;

async fn buffered_copy<R, W>(mut from: R, mut to: W, buffer_size: usize) -> std::io::Result<u64>
where
    R: AsyncRead + std::marker::Unpin,
    W: AsyncWrite + std::marker::Unpin,
{
    let mut buf_from = BufReader::with_capacity(buffer_size, &mut from);
    futures::io::copy_buf(&mut buf_from, &mut to).await
}

fn zx_socket_from_fd(fd: i32) -> Result<fidl::AsyncSocket> {
    let handle = fdio::transfer_fd(unsafe { std::fs::File::from_raw_fd(fd) })?;
    fidl::AsyncSocket::from_socket(fidl::Socket::from(handle))
        .context("making fidl::AsyncSocket from fidl::Socket")
}

fn print_prelude_info(
    message: String,
    status: CompatibilityState,
    platform_abi: u64,
) -> Result<()> {
    let ssh_connection = std::env::var("SSH_CONNECTION")?;
    let info = ConnectionInfo {
        ssh_connection: ssh_connection.clone(),
        compatibility: CompatibilityInfo { status, platform_abi, message: message.clone() },
    };

    let encoded_message = serde_json::to_string(&info)?;
    println!("{encoded_message}");
    Ok(())
}

/// Utility to bridge an overnet/RCS connection via SSH. If you're running this manually, you are
/// probably doing something wrong.
#[derive(FromArgs)]
struct Args {
    /// use circuit-switched connection
    #[argh(switch)]
    circuit: bool,

    /// flag indicating compatibility checking should be performed.
    /// This also has the side effect of returning the $SSH_CONNECTION value in the response.
    /// This is done to keep the remote side response parsing simple and backwards compatible.
    #[argh(option)]
    abi_revision: Option<u64>,

    /// ID number. RCS will reproduce this number once you connect to it. This allows us to
    /// associate an Overnet connection with an RCS connection, in spite of the fuzziness of
    /// Overnet's mesh.
    #[argh(positional)]
    id: Option<u64>,
}

#[fuchsia::main(logging_tags = ["remote_control_runner"])]
async fn main() -> Result<()> {
    let args: Args = argh::from_env();
    // Perform the compatibility checking between the caller (the daemon) and the platform (this program).
    if let Some(abi) = args.abi_revision {
        let daemon_revision = AbiRevision(abi);
        let platform_abi = version_history::get_latest_abi_revision();
        let status: CompatibilityState;
        let message = match check_abi_revision(Some(daemon_revision)) {
            Ok(_) => {
                tracing::info!("Daemon is running supported revision: {daemon_revision}");
                status = CompatibilityState::Supported;
                "Daemon is running supported revision".to_string()
            }
            Err(e) => {
                status = e.clone().into();
                let warning = format!("abi revision {daemon_revision} not supported: {e}");
                tracing::warn!("{warning}");
                warning
            }
        };
        print_prelude_info(message, status, platform_abi)?;
    } else {
        // This is the legacy caller that does not support compatibility checking. Do not write anything
        // to stdout.
        // messages to stderr will cause the pipe to close as well.
        tracing::warn!("--abi-revision not present. Compatibility checks are disabled.");
    }

    let rcs_proxy = connect_to_protocol::<RemoteControlMarker>()?;
    let (local_socket, remote_socket) = fidl::Socket::create_stream();

    if args.circuit {
        rcs_proxy.add_overnet_link(args.id.unwrap_or(0), remote_socket).await?;
    } else {
        return Err(anyhow::format_err!("Legacy overnet is no longer supported."));
    }

    let local_socket = fidl::AsyncSocket::from_socket(local_socket)?;
    let (mut rx_socket, mut tx_socket) = futures::AsyncReadExt::split(local_socket);

    let mut stdin = zx_socket_from_fd(std::io::stdin().lock().as_raw_fd())?;
    let mut stdout = zx_socket_from_fd(std::io::stdout().lock().as_raw_fd())?;

    let in_fut = buffered_copy(&mut stdin, &mut tx_socket, BUFFER_SIZE);
    let out_fut = buffered_copy(&mut rx_socket, &mut stdout, BUFFER_SIZE);
    futures::pin_mut!(in_fut);
    futures::pin_mut!(out_fut);
    match select(in_fut, out_fut).await {
        future::Either::Left((v, _)) => v.context("stdin copy"),
        future::Either::Right((v, _)) => v.context("stdout copy"),
    }
    .map(|_| ())
}

#[cfg(test)]
mod test {

    use {
        super::*,
        anyhow::Error,
        fidl::endpoints::create_proxy_and_stream,
        fidl_fuchsia_developer_remotecontrol::{
            IdentifyHostResponse, RemoteControlMarker, RemoteControlProxy, RemoteControlRequest,
        },
        fuchsia_async as fasync,
        std::cell::RefCell,
        std::rc::Rc,
    };

    async fn send_request(proxy: &RemoteControlProxy, id: Option<u64>) -> Result<()> {
        // If the program was launched with a u64, that's our ffx daemon ID, so add it to RCS.
        // The daemon id is used to map the RCS instance back to an ip address or
        // nodename in the daemon, for target merging.
        if let Some(id) = id {
            proxy.add_id(id).await.with_context(|| format!("Failed to add id {} to RCS", id))
        } else {
            // We just need to make a request to the RCS - it doesn't really matter
            // what we choose here so long as there are no side effects.
            let _ = proxy.identify_host().await?;
            Ok(())
        }
    }

    fn setup_fake_rcs(handle_stream: bool) -> RemoteControlProxy {
        let (proxy, mut stream) = create_proxy_and_stream::<RemoteControlMarker>().unwrap();

        if !handle_stream {
            return proxy;
        }

        fasync::Task::local(async move {
            let last_id = Rc::new(RefCell::new(0));
            while let Ok(req) = stream.try_next().await {
                match req {
                    Some(RemoteControlRequest::IdentifyHost { responder }) => {
                        let _ = responder
                            .send(Ok(&IdentifyHostResponse {
                                nodename: Some("".to_string()),
                                addresses: Some(vec![]),
                                ids: Some(vec![last_id.borrow().clone()]),
                                ..Default::default()
                            }))
                            .unwrap();
                    }
                    Some(RemoteControlRequest::AddId { id, responder }) => {
                        last_id.replace(id);
                        responder.send().unwrap();
                    }
                    _ => assert!(false),
                }
            }
        })
        .detach();

        proxy
    }

    #[fuchsia::test]
    async fn test_handles_successful_response() -> Result<(), Error> {
        let rcs_proxy = setup_fake_rcs(true);
        assert!(send_request(&rcs_proxy, None).await.is_ok());
        Ok(())
    }

    #[fuchsia::test]
    async fn test_handles_failed_response() -> Result<(), Error> {
        let rcs_proxy = setup_fake_rcs(false);
        assert!(send_request(&rcs_proxy, None).await.is_err());
        Ok(())
    }

    #[fuchsia::test]
    async fn test_sends_id_if_given() -> Result<(), Error> {
        let rcs_proxy = setup_fake_rcs(true);
        send_request(&rcs_proxy, Some(34u64)).await.unwrap();
        let ident = rcs_proxy.identify_host().await?.unwrap();
        assert_eq!(34u64, ident.ids.unwrap()[0]);
        Ok(())
    }
}
