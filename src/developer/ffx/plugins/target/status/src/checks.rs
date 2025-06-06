// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use discovery::query::TargetInfoQuery;
use discovery::{DiscoverySources, FastbootConnectionState, TargetHandle, TargetState};
use fdomain_fuchsia_hwinfo::{ProductInfo, ProductProxy};
use ffx_config::EnvironmentContext;
use ffx_diagnostics::{Check, CheckFut};
use ffx_fastboot as _;
use ffx_fastboot_connection_factory::{
    ConnectionFactory, FastbootConnectionFactory, FastbootConnectionKind,
};
use ffx_target::connection::ConnectionError;
use ffx_target::ssh_connector::SshConnector;
use ffx_target::{Connection, TargetConnection, TargetConnectionError, TargetConnector};
use std::io::Write;

pub(crate) struct GetTargetSpecifier<'a, W>(
    pub(crate) &'a EnvironmentContext,
    std::marker::PhantomData<W>,
);

impl<'a, W> GetTargetSpecifier<'a, W> {
    pub fn new(ctx: &'a EnvironmentContext) -> Self {
        Self(ctx, Default::default())
    }
}

impl<W> Check for GetTargetSpecifier<'_, W>
where
    W: Write + Sized,
{
    type Input = ();
    type Output = TargetInfoQuery;
    type Writer = W;

    fn write_preamble(
        &self,
        _input: &Self::Input,
        writer: &mut Self::Writer,
    ) -> std::io::Result<()> {
        write!(writer, "Getting target specifier from config... ")?;
        writer.flush()
    }

    fn on_success(&self, output: &Self::Output, writer: &mut Self::Writer) -> std::io::Result<()> {
        writeln!(writer, " target is {:?}", output)
    }

    fn check<'a>(
        &'a mut self,
        _input: Self::Input,
        _writer: &'a mut Self::Writer,
    ) -> CheckFut<'a, Self::Output> {
        Box::pin(async move { ffx_target::get_target_specifier(self.0).await.map(Into::into) })
    }
}

pub(crate) struct ResolveTarget<'a, W> {
    ctx: &'a EnvironmentContext,
    _writer: std::marker::PhantomData<W>,
}

impl<'a, W> ResolveTarget<'a, W> {
    pub fn new(ctx: &'a EnvironmentContext) -> Self {
        Self { ctx, _writer: Default::default() }
    }
}

// This may need some tweaking if looking at too many/too few sources for each query type.
fn sources_from_query(query: &TargetInfoQuery) -> DiscoverySources {
    match query {
        TargetInfoQuery::NodenameOrSerial(_) | TargetInfoQuery::First => DiscoverySources::all(),
        TargetInfoQuery::Serial(_) => {
            DiscoverySources::USB
                | DiscoverySources::MANUAL
                | DiscoverySources::EMULATOR
                | DiscoverySources::FASTBOOT_FILE
        }
        TargetInfoQuery::VSock(_) => DiscoverySources::USB | DiscoverySources::EMULATOR,
        TargetInfoQuery::Usb(_) => DiscoverySources::USB,
        TargetInfoQuery::Addr(_) => {
            DiscoverySources::FASTBOOT_FILE | DiscoverySources::EMULATOR | DiscoverySources::MANUAL
        }
    }
}

impl<W> Check for ResolveTarget<'_, W>
where
    W: Write + Sized,
{
    type Input = TargetInfoQuery;
    type Output = TargetHandle;
    type Writer = W;

    fn write_preamble(
        &self,
        input: &Self::Input,
        writer: &mut Self::Writer,
    ) -> std::io::Result<()> {
        let sources = sources_from_query(&input);
        let sources =
            sources.iter_names().map(|(n, _)| n.to_owned()).collect::<Vec<_>>().join(", ");
        write!(writer, "Attempting to find device {:?} via {}... ", input, sources)?;
        writer.flush()
    }

    fn on_success(&self, output: &Self::Output, writer: &mut Self::Writer) -> std::io::Result<()> {
        writeln!(writer, "Target resolved to {:?}", output)
    }

    fn check<'a>(
        &'a mut self,
        input: Self::Input,
        _writer: &'a mut Self::Writer,
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

pub(crate) struct ConnectSsh<'a, W> {
    ctx: &'a EnvironmentContext,
    _w: std::marker::PhantomData<W>,
}

impl<'a, W> ConnectSsh<'a, W> {
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

impl<W> Check for ConnectSsh<'_, W>
where
    W: Write + Sized,
{
    type Input = TargetHandle;
    type Output = Connection;
    type Writer = W;

    fn write_preamble(
        &self,
        input: &Self::Input,
        writer: &mut Self::Writer,
    ) -> std::io::Result<()> {
        writeln!(writer, "Attempting to connect ssh to device {:?}", input)
    }

    fn on_success(&self, _output: &Self::Output, writer: &mut Self::Writer) -> std::io::Result<()> {
        writeln!(writer, "Connected")
    }

    fn check<'a>(
        &'a mut self,
        input: Self::Input,
        writer: &'a mut Self::Writer,
    ) -> CheckFut<'a, Self::Output> {
        Box::pin(async {
            let resolution = ffx_target::Resolution::from_target_handle(input)?;
            // Probably want to report the address to which we're connecting.
            let connector = ConnectorHolder(SshConnector::new(
                netext::ScopedSocketAddr::from_socket_addr(resolution.addr()?)?,
                self.ctx,
            )?);
            writeln!(writer, "Executing the command: `{}`", connector.0.fdomain_command()?)?;
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

pub(crate) struct ConnectRemoteControlProxy<W> {
    pub(crate) timeout: std::time::Duration,
    _w: std::marker::PhantomData<W>,
}

impl<W> ConnectRemoteControlProxy<W> {
    pub fn new(timeout: std::time::Duration) -> Self {
        Self { timeout, _w: Default::default() }
    }
}

impl<W> Check for ConnectRemoteControlProxy<W>
where
    W: Write + Sized,
{
    type Input = Connection;
    type Output = ProductInfo;
    type Writer = W;

    fn write_preamble(
        &self,
        _input: &Self::Input,
        writer: &mut Self::Writer,
    ) -> std::io::Result<()> {
        write!(writer, "Attempting to connect remote control through ssh... ")?;
        writer.flush()
    }

    fn on_success(&self, _output: &Self::Output, writer: &mut Self::Writer) -> std::io::Result<()> {
        writeln!(writer, "Success")
    }

    fn check<'a>(
        &'a mut self,
        input: Self::Input,
        _writer: &'a mut Self::Writer,
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

pub(crate) struct FastbootDeviceStatus<W>(std::marker::PhantomData<W>);

impl<W> FastbootDeviceStatus<W> {
    pub fn new() -> Self {
        Self(Default::default())
    }
}

impl<W> Check for FastbootDeviceStatus<W>
where
    W: Write + Sized,
{
    type Input = TargetHandle;
    type Output = String;
    type Writer = W;

    fn write_preamble(
        &self,
        input: &Self::Input,
        writer: &mut Self::Writer,
    ) -> std::io::Result<()> {
        write!(writer, "Attempting to connect to Fastboot device: {input:?}... ")?;
        writer.flush()
    }

    fn check<'a>(
        &'a mut self,
        input: Self::Input,
        _writer: &'a mut Self::Writer,
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
