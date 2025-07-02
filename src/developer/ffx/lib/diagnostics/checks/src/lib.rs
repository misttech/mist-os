// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use discovery::query::TargetInfoQuery;
use discovery::{DiscoverySources, FastbootConnectionState, TargetHandle, TargetState};
use fdomain_fuchsia_hwinfo::{ProductInfo, ProductProxy};
use ffx_config::EnvironmentContext;
use ffx_diagnostics::{Check, CheckExt, CheckFut, Notifier};
use ffx_fastboot as _;
use ffx_fastboot_connection_factory::{
    ConnectionFactory, FastbootConnectionFactory, FastbootConnectionKind,
};
use ffx_target::connection::ConnectionError;
use ffx_target::ssh_connector::SshConnector;
use ffx_target::{Connection, TargetConnection, TargetConnectionError, TargetConnector};
use std::time::Duration;

pub async fn run_diagnostics_with_handle<N>(
    env_context: &EnvironmentContext,
    target_handle: TargetHandle,
    notifier: &mut N,
    product_timeout: Duration,
) -> fho::Result<()>
where
    N: Notifier + std::marker::Unpin,
{
    match target_handle.state {
        TargetState::Product { .. } => {
            check_product_device(env_context, notifier, target_handle, product_timeout).await?
        }
        TargetState::Fastboot(_) => check_fastboot_device(notifier, target_handle).await?,
        TargetState::Unknown => {
            fho::return_user_error!("Device is in an unknown state. No way to check status.")
        }
        TargetState::Zedboot => {
            fho::return_user_error!("Zedboot is not currently supported for this command.")
        }
    }
    notifier.on_success("All checks passed.")?;
    Ok(())
}

pub async fn run_diagnostics<N>(
    env: &EnvironmentContext,
    notifier: &mut N,
    product_timeout: Duration,
) -> fho::Result<()>
where
    N: Notifier + std::marker::Unpin,
{
    let (target, notifier) = GetTargetSpecifier::new(&env)
        .check_with_notifier((), notifier)
        .and_then_check(ResolveTarget::new(&env))
        .await?;

    run_diagnostics_with_handle(&env, target, notifier, product_timeout).await?;
    Ok(())
}

async fn check_product_device<N>(
    env_context: &EnvironmentContext,
    notifier: &mut N,
    device: TargetHandle,
    timeout: Duration,
) -> fho::Result<()>
where
    N: Notifier + std::marker::Unpin,
{
    // Depending on the number of targets resolved and their types,
    // this could go one of several ways. It may also be nice to mention where the devices
    // originated. This does not check VSock devices.
    let (info, notifier) = ConnectSsh::new(env_context)
        .check_with_notifier(device, notifier)
        .and_then_check(ConnectRemoteControlProxy::new(timeout))
        .await
        .map_err(|e| fho::Error::User(e.into()))?;
    notifier.on_success(format!("Got device info: {:?}", info))?;
    Ok(())
}

async fn check_fastboot_device<N>(notifier: &mut N, device: TargetHandle) -> fho::Result<()>
where
    N: Notifier + std::marker::Unpin,
{
    let (info, notifier) = FastbootDeviceStatus::new()
        .check_with_notifier(device, notifier)
        .await
        .map_err(|e| fho::Error::User(e.into()))?;
    notifier.on_success(format!("Got device info: {:?}", info))?;
    Ok(())
}

pub struct GetTargetSpecifier<'a, N>(
    pub(crate) &'a EnvironmentContext,
    std::marker::PhantomData<N>,
);

impl<'a, N> GetTargetSpecifier<'a, N> {
    pub fn new(ctx: &'a EnvironmentContext) -> Self {
        Self(ctx, Default::default())
    }
}

impl<N> Check for GetTargetSpecifier<'_, N>
where
    N: Notifier + Sized,
{
    type Input = ();
    type Output = TargetInfoQuery;
    type Notifier = N;

    fn write_preamble(
        &self,
        _input: &Self::Input,
        notifier: &mut Self::Notifier,
    ) -> anyhow::Result<()> {
        notifier.info("Getting target specifier from config... ")
    }

    fn on_success(
        &self,
        output: &Self::Output,
        notifier: &mut Self::Notifier,
    ) -> anyhow::Result<()> {
        notifier.on_success(format!("Target is {:?}", output))
    }

    fn check<'a>(
        &'a mut self,
        _input: Self::Input,
        _notifier: &'a mut Self::Notifier,
    ) -> CheckFut<'a, Self::Output> {
        Box::pin(async move { ffx_target::get_target_specifier(self.0).await.map(Into::into) })
    }
}

pub struct ResolveTarget<'a, N> {
    ctx: &'a EnvironmentContext,
    _notifier: std::marker::PhantomData<N>,
}

impl<'a, N> ResolveTarget<'a, N> {
    pub fn new(ctx: &'a EnvironmentContext) -> Self {
        Self { ctx, _notifier: Default::default() }
    }
}

// This may need some tweaking if looking at too many/too few sources for each query type.
fn sources_from_query(query: &TargetInfoQuery) -> DiscoverySources {
    match query {
        TargetInfoQuery::NodenameOrSerial(_)
        | TargetInfoQuery::First
        | TargetInfoQuery::Serial(_) => DiscoverySources::all(),
        TargetInfoQuery::VSock(_) => DiscoverySources::USB | DiscoverySources::EMULATOR,
        TargetInfoQuery::Usb(_) => DiscoverySources::USB,
        TargetInfoQuery::Addr(_) => {
            DiscoverySources::MDNS
                | DiscoverySources::FASTBOOT_FILE
                | DiscoverySources::EMULATOR
                | DiscoverySources::MANUAL
        }
    }
}

impl<N> Check for ResolveTarget<'_, N>
where
    N: Notifier + Sized,
{
    type Input = TargetInfoQuery;
    type Output = TargetHandle;
    type Notifier = N;

    fn write_preamble(
        &self,
        input: &Self::Input,
        notifier: &mut Self::Notifier,
    ) -> anyhow::Result<()> {
        let sources = sources_from_query(&input);
        let sources =
            sources.iter_names().map(|(n, _)| n.to_owned()).collect::<Vec<_>>().join(", ");
        notifier.info(format!("Attempting to find device {:?} via {}... ", input, sources))
    }

    fn on_success(
        &self,
        output: &Self::Output,
        notifier: &mut Self::Notifier,
    ) -> anyhow::Result<()> {
        notifier.on_success(format!("Target resolved to {:?}", output))
    }

    fn check<'a>(
        &'a mut self,
        input: Self::Input,
        _notifier: &'a mut Self::Notifier,
    ) -> CheckFut<'a, Self::Output> {
        // This step, on account of it being the most broad, will have the most potential ways to
        // fail, meaning that the solution space is quite wide. If this fails and there are no
        // devices found, then that means there are subsequently a great number of ways to resolve
        // the device. What we can perhaps do instead of discovering in parallel is to go serially,
        // with each failure to discover devices leading to a specific error.
        //
        // There should also not be certain errors if the device is formatted a given way. For
        // example: if the device is an IP address, we should not attempt to resolve the device via
        // mDNS, as we already have the IP address.
        Box::pin(async {
            let sources = sources_from_query(&input);
            // There should be some kind of error here if the device resolves to an empty array.
            let mut targets =
                ffx_target::resolve_target_query_with_sources(input, self.ctx, sources).await?;
            if targets.is_empty() {
                return Err(anyhow::anyhow!("Unable to find any devices"));
            }
            if targets.len() > 1 {
                return Err(anyhow::anyhow!("Too many targets. You may need to be more specific in the device you are checking. Found: {targets:?}"));
            }
            Ok(targets.pop().unwrap())
        })
    }
}

pub struct ConnectSsh<'a, N> {
    ctx: &'a EnvironmentContext,
    _w: std::marker::PhantomData<N>,
}

impl<'a, N> ConnectSsh<'a, N> {
    pub fn new(ctx: &'a EnvironmentContext) -> Self {
        Self { ctx, _w: Default::default() }
    }
}

#[derive(Debug)]
struct ConnectorHolder(SshConnector);

impl TargetConnector for ConnectorHolder {
    const CONNECTION_TYPE: &'static str = "ssh";

    async fn connect(&mut self) -> Result<TargetConnection, TargetConnectionError> {
        Ok(TargetConnection::FDomain(self.0.connect_via_fdomain().await?))
    }
}

impl<N> Check for ConnectSsh<'_, N>
where
    N: Notifier + Sized,
{
    type Input = TargetHandle;
    type Output = Connection;
    type Notifier = N;

    fn write_preamble(
        &self,
        input: &Self::Input,
        notifier: &mut Self::Notifier,
    ) -> anyhow::Result<()> {
        notifier.info(format!("Attempting to connect ssh to device {:?}", input))
    }

    fn on_success(
        &self,
        _output: &Self::Output,
        notifier: &mut Self::Notifier,
    ) -> anyhow::Result<()> {
        notifier.on_success("Connected")
    }

    fn check<'a>(
        &'a mut self,
        input: Self::Input,
        notifier: &'a mut Self::Notifier,
    ) -> CheckFut<'a, Self::Output> {
        Box::pin(async {
            let resolution = ffx_target::Resolution::from_target_handle(input)?;
            // Probably want to report the address to which we're connecting.
            let connector = ConnectorHolder(SshConnector::new(
                netext::ScopedSocketAddr::from_socket_addr(resolution.addr()?)?,
                self.ctx,
            )?);
            notifier
                .info(format!("Executing the command: `{}`", connector.0.fdomain_command()?))?;
            Connection::new(connector).await.map_err(|e| {
                anyhow::anyhow!(
                    "Unable to connect to device. Underlying error: {}",
                    match e {
                        ConnectionError::ConnectionStartError(_, s) => anyhow::anyhow!("{}", s),
                        _ => e.into(),
                    }
                )
            })
        })
    }
}

pub struct ConnectRemoteControlProxy<N> {
    pub timeout: std::time::Duration,
    _w: std::marker::PhantomData<N>,
}

impl<N> ConnectRemoteControlProxy<N> {
    pub fn new(timeout: std::time::Duration) -> Self {
        Self { timeout, _w: Default::default() }
    }
}

impl<N> Check for ConnectRemoteControlProxy<N>
where
    N: Notifier + Sized,
{
    type Input = Connection;
    type Output = ProductInfo;
    type Notifier = N;

    fn write_preamble(
        &self,
        _input: &Self::Input,
        notifier: &mut Self::Notifier,
    ) -> anyhow::Result<()> {
        notifier.info("Attempting to connect remote control through ssh... ")
    }

    fn on_success(
        &self,
        _output: &Self::Output,
        notifier: &mut Self::Notifier,
    ) -> anyhow::Result<()> {
        notifier.on_success("Success")
    }

    fn check<'a>(
        &'a mut self,
        input: Self::Input,
        _notifier: &'a mut Self::Notifier,
    ) -> CheckFut<'a, Self::Output> {
        Box::pin(async move {
            let proxy = input.rcs_proxy_fdomain().await?;
            let product_proxy: ProductProxy = target_holders::fdomain::open_moniker_fdomain(
                &proxy,
                rcs::OpenDirType::ExposedDir,
                "/core/hwinfo",
                self.timeout,
            )
            .await?;
            let info = product_proxy.get_info().await?;
            Ok(info)
        })
    }
}

pub struct FastbootDeviceStatus<N>(std::marker::PhantomData<N>);

impl<N> FastbootDeviceStatus<N> {
    pub fn new() -> Self {
        Self(Default::default())
    }
}

impl<N> Check for FastbootDeviceStatus<N>
where
    N: Notifier + Sized,
{
    type Input = TargetHandle;
    type Output = String;
    type Notifier = N;

    fn write_preamble(
        &self,
        input: &Self::Input,
        notifier: &mut Self::Notifier,
    ) -> anyhow::Result<()> {
        notifier.info(format!("Attempting to connect to Fastboot device: {input:?}... "))
    }

    fn check<'a>(
        &'a mut self,
        input: Self::Input,
        _notifier: &'a mut Self::Notifier,
    ) -> CheckFut<'a, Self::Output> {
        Box::pin(async {
            // Example handle: [TargetHandle { node_name: None, state: Fastboot(FastbootTargetState
            // { serial_number: "", connection_state: Tcp([TargetIpAddr(127.0.0.1:38957)]) }),
            // manual: false, orig
            // in: FastbootTcp }]
            let fastboot_state = match input.state {
                TargetState::Fastboot(ref s) => s,
                _ => panic!("received non-fastboot target handle: {input:?}"),
            };
            let connection_kind = match &fastboot_state.connection_state {
                FastbootConnectionState::Usb => {
                    let discovery::TargetState::Fastboot(discovery::FastbootTargetState {
                        serial_number,
                        ..
                    }) = input.state
                    else {
                        panic!("input in incorrect state. Expecting Fastboot state instead found: {input:?}");
                    };
                    FastbootConnectionKind::Usb(serial_number)
                }
                // This assumes the first address in the array will a.) exist, and b.) be the _most
                // correct_ address from which we're selecting.
                FastbootConnectionState::Tcp(v) => FastbootConnectionKind::Tcp(
                    input.node_name.unwrap_or_else(|| "".to_owned()),
                    v[0].into(),
                ),
                FastbootConnectionState::Udp(v) => FastbootConnectionKind::Udp(
                    input.node_name.unwrap_or_else(|| "".to_owned()),
                    v[0].into(),
                ),
            };
            let factory = ConnectionFactory {};
            let mut interface = factory.build_interface(connection_kind).await?;
            interface.get_var("serialno").await.map_err(Into::into)
        })
    }
}
