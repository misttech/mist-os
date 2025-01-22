// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::Context;
use argh::{ArgsInfo, FromArgs};
use emulator_instance::EMU_INSTANCE_ROOT_DIR;
use ffx_config::EnvironmentContext;
use ffx_emulator_config::ShowDetail;
use ffx_emulator_engines::EngineBuilder;
use fho::{return_bug, FfxContext, Result};
use fidl_fuchsia_developer_ffx::TargetInfo;
use futures::io::AsyncReadExt;
use futures::stream::StreamExt;
use futures::FutureExt;
use netext::{MultithreadedTokioAsyncWrapper, TcpListenerStream, TokioAsyncReadExt};
use signal_hook::consts::signal::SIGINT;
use signal_hook::iterator::Signals;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener as SyncTcpListener};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use target_connector::Connector;
use tokio::net::{TcpListener, TcpStream};
use tracing::info;
use {
    fidl_fuchsia_developer_remotecontrol as rc, fidl_fuchsia_starnix_container as fstarcontainer,
    fuchsia_async as fasync,
};

use crate::common::*;

const ADB_DEFAULT_PORT: u16 = 5555;

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "adb",
    example = "ffx starnix adb proxy",
    description = "Bridge from host adb to adbd running inside starnix"
)]
pub struct StarnixAdbCommand {
    /// path to the adb client command
    #[argh(option, default = "String::from(\"adb\")")]
    adb: String,
    #[argh(subcommand)]
    subcommand: AdbSubcommand,
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand)]
enum AdbSubcommand {
    Connect(AdbConnectArgs),
    Proxy(AdbProxyArgs),
}

impl StarnixAdbCommand {
    pub async fn run(
        &self,
        context: &EnvironmentContext,
        rcs_connector: &Connector<rc::RemoteControlProxy>,
        target_info: fho::Result<TargetInfo>,
    ) -> Result<()> {
        match &self.subcommand {
            AdbSubcommand::Connect(args) => {
                args.run_connect(context, &self.adb, &target_info?).await
            }
            AdbSubcommand::Proxy(args) => args.run_proxy(&self.adb, rcs_connector).await,
        }
    }
}

/// directly connect the local adb server to an adbd instance running on the target.
#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "connect")]
struct AdbConnectArgs {}

impl AdbConnectArgs {
    async fn run_connect(
        &self,
        context: &EnvironmentContext,
        adb: &str,
        target_info: &TargetInfo,
    ) -> Result<()> {
        let Some(address) = target_info.ssh_address.as_ref() else {
            return_bug!("Target does not appear to have an ssh address.");
        };

        let ssh_address: TargetAddr = address.into();
        let ssh_address: SocketAddr = ssh_address.into();

        let adb_port = if ssh_address.port() == 22 || ssh_address.port() == 8022 {
            // This only covers `--net tap` emulators, physical devices with CDC ethernet, and
            // funnel. funnel also forwards adb to local 5555, so it's fine to use the default.
            ADB_DEFAULT_PORT
        } else {
            // If the device doesn't have a standard SSH port, it's likely an emulator
            // instance with user networking. Look up the instance and get its host port mapping
            // to find the port we need.
            let nodename = target_info.nodename.as_ref().bug_context("getting target name")?;
            get_emu_host_adb_port(context, &nodename)
                .await
                .bug_context("Finding host adb port for user networking emulator")?
        };

        let mut adb_address = ssh_address.clone();
        adb_address.set_port(adb_port);

        let mut adb_cmd = Command::new(adb);
        adb_cmd.arg("connect").arg(adb_address.to_string());
        info!("running `{adb_cmd:?}`");

        let connect_res = adb_cmd.output().bug_context("running adb connect")?;
        if !connect_res.status.success() {
            let stdout = String::from_utf8_lossy(&connect_res.stdout);
            let stderr = String::from_utf8_lossy(&connect_res.stderr);
            return_bug!("Couldn't run adb connect. stdout={stdout} stderr={stderr}");
        }

        eprintln!("adb is connected! You may need to run `export ANDROID_SERIAL={adb_address}`.");
        Ok(())
    }
}

async fn get_emu_host_adb_port(context: &EnvironmentContext, name: &str) -> Result<u16> {
    let instance_dir: PathBuf =
        context.get(EMU_INSTANCE_ROOT_DIR).bug_context("getting emulator instance dir")?;
    let emu_instances = emulator_instance::EmulatorInstances::new(instance_dir);
    let builder = EngineBuilder::new(emu_instances);
    let mut instance_name = Some(name.to_string());
    let engine = builder
        .get_engine_by_name(&mut instance_name)
        .with_bug_context(|| format!("getting emulator engine for target {name}"))?
        .bug_context("have configured emulator but no engine was returned")?;
    let mut details = engine.show(vec![ShowDetail::Net {
        mac_address: Default::default(),
        mode: Default::default(),
        ports: Default::default(),
        upscript: Default::default(),
    }]);
    if details.len() != 1 {
        return_bug!("expected emulator details of length 1, got {details:?}");
    }
    match details.remove(0) {
        ShowDetail::Net { ports, .. } => {
            let ports = ports.bug_context("getting host port mappings")?;
            let adb_mapping = ports.get("adb").bug_context("getting adb port mapping")?;
            adb_mapping.host.bug_context("getting adb host port")
        }
        unexpected => return_bug!("asked for networking details, got {unexpected:?}"),
    }
}

async fn serve_adb_connection(
    mut stream: MultithreadedTokioAsyncWrapper<TcpStream>,
    bridge_socket: fidl::Socket,
) -> anyhow::Result<()> {
    let mut bridge = fidl::AsyncSocket::from_socket(bridge_socket);
    let (breader, mut bwriter) = (&mut bridge).split();
    let (sreader, mut swriter) = (&mut stream).split();

    let copy_result = futures::select! {
        r = futures::io::copy(breader, &mut swriter).fuse() => {
            r
        },
        r = futures::io::copy(sreader, &mut bwriter).fuse() => {
            r
        },
    };

    copy_result.map(|_| ()).map_err(|e| e.into())
}

fn find_open_port(start: u16) -> u16 {
    let mut addr = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), start);

    info!("probing for an open port for the adb bridge...");
    loop {
        info!("probing {addr:?}...");
        match SyncTcpListener::bind(addr) {
            Ok(_) => {
                info!("{addr:?} appears to be available");
                return addr.port();
            }
            Err(e) => {
                info!("{addr:?} appears unavailable: {e:?}");
                addr.set_port(
                    addr.port().checked_add(1).expect("should find open port before overflow"),
                );
            }
        }
    }
}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "proxy",
    description = "Bridge from host adb to adbd running inside starnix"
)]
struct AdbProxyArgs {
    /// the moniker of the container running adbd
    /// (defaults to looking for a container in the current session)
    #[argh(option, short = 'm')]
    pub moniker: Option<String>,

    /// which port to serve the adb server on
    #[argh(option, short = 'p', default = "find_open_port(5556)")]
    pub port: u16,

    /// disable automatically running "adb connect"
    #[argh(switch)]
    pub no_autoconnect: bool,
}

impl AdbProxyArgs {
    async fn run_proxy(
        &self,
        adb: &str,
        rcs_connector: &Connector<rc::RemoteControlProxy>,
    ) -> Result<()> {
        let reconnect = || async {
            let rcs_proxy = connect_to_rcs(rcs_connector).await?;
            anyhow::Ok((
                connect_to_contoller(&rcs_proxy, self.moniker.clone()).await?,
                Arc::new(AtomicBool::new(false)),
            ))
        };
        let mut controller_proxy = reconnect().await?;

        let mut signals = Signals::new(&[SIGINT]).unwrap();
        let handle = signals.handle();
        let signal_thread = std::thread::spawn(move || {
            if let Some(signal) = signals.forever().next() {
                assert_eq!(signal, SIGINT);
                eprintln!("Caught interrupt. Shutting down starnix adb bridge...");
                std::process::exit(0);
            }
        });

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, self.port))
            .await
            .expect("cannot bind to adb address");
        let listen_address = listener.local_addr().expect("cannot get adb server address");

        if !self.no_autoconnect {
            // It's necessary to run adb connect on a separate thread so it doesn't block the async
            // executor running the socket listener.
            let adb_path = adb.to_string();
            std::thread::spawn(move || {
                let mut adb_command = Command::new(&adb_path);
                adb_command.arg("connect").arg(listen_address.to_string());
                match adb_command.status() {
                    Ok(_) => {}
                    Err(io_err) if io_err.kind() == ErrorKind::NotFound => {
                        panic!("Could not find adb binary named `{adb_path}`. If your adb is not in your $PATH, use the --adb flag to specify where to find it.");
                    }
                    Err(io_err) => {
                        panic!("Failed to run `${adb_command:?}`: {io_err:?}");
                    }
                }
            });
        } else {
            println!("ADB bridge started. To connect: adb connect {listen_address}");
        }
        let mut listener = TcpListenerStream(listener);
        while let Some(stream) = listener.next().await {
            if controller_proxy.1.load(Ordering::SeqCst) {
                controller_proxy = reconnect().await?;
            }

            let stream = stream.map_err(|e| fho::Error::Unexpected(e.into()))?;
            let (sbridge, cbridge) = fidl::Socket::create_stream();

            controller_proxy
                .0
                .vsock_connect(fstarcontainer::ControllerVsockConnectRequest {
                    port: Some(ADB_DEFAULT_PORT as u32),
                    bridge_socket: Some(sbridge),
                    ..Default::default()
                })
                .context("connecting to adbd")?;

            let reconnect_flag = Arc::clone(&controller_proxy.1);
            fasync::Task::spawn(async move {
                serve_adb_connection(stream.into_multithreaded_futures_stream(), cbridge)
                    .await
                    .unwrap_or_else(|e| println!("serve_adb_connection returned with {:?}", e));
                reconnect_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .detach();
        }

        handle.close();
        signal_thread.join().expect("signal thread to shutdown without panic");
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use futures::AsyncWriteExt;
    use netext::{TcpListenerStream, TokioAsyncReadExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn run_connection(listener: TcpListener, socket: fidl::Socket) {
        let mut listener = TcpListenerStream(listener);
        if let Some(stream) = listener.next().await {
            let stream = stream.unwrap();
            serve_adb_connection(stream.into_multithreaded_futures_stream(), socket).await.unwrap();
        } else {
            panic!("did not get a connection");
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_adb_relay() {
        let any_local_address = "127.0.0.1:0";
        let listener = TcpListener::bind(any_local_address).await.unwrap();
        let local_address = listener.local_addr().unwrap();

        let port = local_address.port();

        let (sbridge, cbridge) = fidl::Socket::create_stream();

        fasync::Task::spawn(async move {
            run_connection(listener, sbridge).await;
        })
        .detach();

        let connect_address = format!("127.0.0.1:{}", port);
        let mut stream = TcpStream::connect(connect_address).await.unwrap().into_futures_stream();

        let test_data_1: Vec<u8> = vec![1, 2, 3, 4, 5];
        stream.write_all(&test_data_1).await.unwrap();

        let mut buf = [0u8; 64];
        let mut async_socket = fidl::AsyncSocket::from_socket(cbridge);
        let bytes_read = async_socket.read(&mut buf).await.unwrap();
        assert_eq!(test_data_1.len(), bytes_read);
        for (a, b) in test_data_1.iter().zip(buf[..bytes_read].iter()) {
            assert_eq!(a, b);
        }

        let test_data_2: Vec<u8> = vec![6, 7, 8, 9, 10, 11];
        let bytes_written = async_socket.write(&test_data_2).await.unwrap();
        assert_eq!(bytes_written, test_data_2.len());

        let mut buf = [0u8; 64];
        let bytes_read = stream.read(&mut buf).await.unwrap();
        assert_eq!(bytes_written, bytes_written);

        for (a, b) in test_data_2.iter().zip(buf[..bytes_read].iter()) {
            assert_eq!(a, b);
        }
    }
}
