// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use discovery::query::TargetInfoQuery;
use discovery::{DiscoverySources, FastbootConnectionState, TargetHandle, TargetState};
use fdomain_fuchsia_hwinfo::{ProductInfo, ProductProxy};
use ffx_config::EnvironmentContext;
use ffx_diagnostics::{Check, CheckExt, CheckFut, Notifier};
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
    let conn_provider = DefaultSshConnectorProvider;
    let (info, notifier) = ConnectSsh::new(env_context, &conn_provider)
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
    let factory = ConnectionFactory {};
    let (info, notifier) = FastbootDeviceStatus::new(&factory)
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

pub trait SshConnectorProvider {
    fn connector_for_target<N>(
        &self,
        ctx: EnvironmentContext,
        handle: TargetHandle,
        notifier: &mut N,
    ) -> anyhow::Result<impl TargetConnector + 'static>
    where
        N: Notifier + Sized;
}

struct DefaultSshConnectorProvider;

impl SshConnectorProvider for DefaultSshConnectorProvider {
    fn connector_for_target<N>(
        &self,
        ctx: EnvironmentContext,
        handle: TargetHandle,
        notifier: &mut N,
    ) -> anyhow::Result<impl TargetConnector + 'static>
    where
        N: Notifier + Sized,
    {
        let resolution = ffx_target::Resolution::from_target_handle(handle)?;
        // Probably want to report the address to which we're connecting.
        let connector = ConnectorHolder(SshConnector::new(
            netext::ScopedSocketAddr::from_socket_addr(resolution.addr()?)?,
            &ctx,
        )?);
        notifier.info(format!("Executing the command: `{}`", connector.0.fdomain_command()?))?;
        Ok(connector)
    }
}

pub struct ConnectSsh<'a, N, P> {
    ctx: &'a EnvironmentContext,
    conn_provider: &'a P,
    _w: std::marker::PhantomData<N>,
}

impl<'a, N, P> ConnectSsh<'a, N, P> {
    pub fn new(ctx: &'a EnvironmentContext, conn_provider: &'a P) -> Self {
        Self { ctx, _w: Default::default(), conn_provider }
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

impl<N, P> Check for ConnectSsh<'_, N, P>
where
    N: Notifier + Sized,
    P: SshConnectorProvider + Sized,
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
            let connector =
                self.conn_provider.connector_for_target(self.ctx.clone(), input, notifier)?;
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

pub struct FastbootDeviceStatus<'a, F, N> {
    factory: &'a F,
    _w: std::marker::PhantomData<N>,
}

impl<'a, F, N> FastbootDeviceStatus<'a, F, N> {
    pub fn new(factory: &'a F) -> Self {
        Self { factory, _w: Default::default() }
    }
}

impl<F, N> Check for FastbootDeviceStatus<'_, F, N>
where
    N: Notifier + Sized,
    F: FastbootConnectionFactory,
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
                _ => return Err(anyhow::anyhow!("received non-fastboot target handle: {input:?}")),
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
            let mut interface = self.factory.build_interface(connection_kind).await?;
            interface.get_var("serialno").await.map_err(Into::into)
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use addr::TargetIpAddr;
    use fdomain_client::fidl::DiscoverableProtocolMarker;
    use fdomain_fuchsia_developer_remotecontrol::RemoteControlMarker;
    use ffx_fastboot_connection_factory::test::setup_connection_factory;
    use ffx_target::FDomainConnection;
    use fidl_fuchsia_developer_remotecontrol as rcs;
    use fidl_fuchsia_hwinfo::{ProductInfo, ProductMarker, ProductRequest};
    use fuchsia_async::Task;
    use futures_lite::stream::StreamExt;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;

    #[fuchsia::test]
    async fn test_fastboot_check_tcp() {
        let (state, factory) = setup_connection_factory();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 1234);
        let handle = TargetHandle {
            node_name: Some("test-node".to_string()),
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "test-serial".to_string(),
                connection_state: FastbootConnectionState::Tcp(vec![TargetIpAddr::from(addr)]),
            }),
            manual: false,
        };
        let mut notifier = ffx_diagnostics::StringNotifier::new();
        let fake_serial = "fake-serial-number".to_string();
        state.lock().unwrap().set_var("serialno".to_string(), fake_serial.clone());
        let mut check = FastbootDeviceStatus::new(&factory);
        let res = check.check(handle, &mut notifier).await.unwrap();
        assert_eq!(res, fake_serial);
    }

    #[fuchsia::test]
    async fn test_fastboot_check_udp() {
        let (state, factory) = setup_connection_factory();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 1234);
        let handle = TargetHandle {
            node_name: Some("test-node".to_string()),
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "test-serial".to_string(),
                connection_state: FastbootConnectionState::Udp(vec![TargetIpAddr::from(addr)]),
            }),
            manual: false,
        };
        let mut notifier = ffx_diagnostics::StringNotifier::new();
        let fake_serial = "fake-serial-number".to_string();
        state.lock().unwrap().set_var("serialno".to_string(), fake_serial.clone());
        let mut check = FastbootDeviceStatus::new(&factory);
        let res = check.check(handle, &mut notifier).await.unwrap();
        assert_eq!(res, fake_serial);
    }

    #[fuchsia::test]
    async fn test_fastboot_check_usb() {
        let (state, factory) = setup_connection_factory();
        let handle = TargetHandle {
            node_name: Some("test-node".to_string()),
            state: TargetState::Fastboot(discovery::FastbootTargetState {
                serial_number: "test-serial".to_string(),
                connection_state: FastbootConnectionState::Usb,
            }),
            manual: false,
        };
        let mut notifier = ffx_diagnostics::StringNotifier::new();
        let fake_serial = "fake-serial-number".to_string();
        state.lock().unwrap().set_var("serialno".to_string(), fake_serial.clone());
        let mut check = FastbootDeviceStatus::new(&factory);
        let res = check.check(handle, &mut notifier).await.unwrap();
        assert_eq!(res, fake_serial);
    }

    #[fuchsia::test]
    async fn test_fastboot_check_failure() {
        let (state, factory) = setup_connection_factory();
        let handle = TargetHandle {
            node_name: Some("test-node".to_string()),
            state: TargetState::Unknown,
            manual: false,
        };
        let mut notifier = ffx_diagnostics::StringNotifier::new();
        let fake_serial = "fake-serial-number".to_string();
        state.lock().unwrap().set_var("serialno".to_string(), fake_serial);
        let mut check = FastbootDeviceStatus::new(&factory);
        let res = check.check(handle, &mut notifier).await;
        assert!(res.is_err());
    }

    #[derive(Debug)]
    struct MockSshConnectorProvider<R> {
        res: RefCell<Option<anyhow::Result<R>>>,
    }

    impl<R> MockSshConnectorProvider<R> {
        fn with_res(res: anyhow::Result<R>) -> Self {
            Self { res: RefCell::new(Some(res)) }
        }
    }

    impl<R> SshConnectorProvider for MockSshConnectorProvider<R>
    where
        R: TargetConnector + 'static,
    {
        fn connector_for_target<N>(
            &self,
            _ctx: EnvironmentContext,
            _handle: TargetHandle,
            _notifier: &mut N,
        ) -> anyhow::Result<impl TargetConnector + 'static>
        where
            N: Notifier + Sized,
        {
            self.res.borrow_mut().take().expect("called `connector_for_target` once").into()
        }
    }

    struct MockConnector {
        results: RefCell<VecDeque<Result<TargetConnection, TargetConnectionError>>>,
    }

    impl std::fmt::Debug for MockConnector {
        fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(fmt, "MockConnector {{}}")
        }
    }

    impl MockConnector {
        /// Delivers the results one at a time in order declared.
        fn with_results(
            res: impl Into<Vec<Result<TargetConnection, TargetConnectionError>>>,
        ) -> Self {
            Self { results: RefCell::new(res.into().into()) }
        }
    }

    impl TargetConnector for MockConnector {
        const CONNECTION_TYPE: &'static str = "mock";

        async fn connect(&mut self) -> Result<TargetConnection, TargetConnectionError> {
            self.results.borrow_mut().pop_front().expect("should have more mocked results left")
        }
    }

    #[fuchsia::test]
    async fn test_connect_ssh() {
        let env = ffx_config::test_init().await.unwrap();
        let m = MockSshConnectorProvider::with_res(Ok(MockConnector::with_results([Ok(
            TargetConnection::FDomain(FDomainConnection::invalid()),
        )])));
        let mut notifier = ffx_diagnostics::StringNotifier::new();
        let mut check = ConnectSsh::new(&env.context, &m);
        let handle = TargetHandle {
            node_name: Some("test-node".to_string()),
            state: TargetState::Unknown,
            manual: false,
        };
        let res = check.check(handle, &mut notifier).await;
        assert!(res.is_ok());
    }

    #[fuchsia::test]
    async fn test_connect_ssh_failures() {
        let env = ffx_config::test_init().await.unwrap();
        let m = MockSshConnectorProvider::<MockConnector>::with_res(Err(anyhow::anyhow!(
            "Couldn't get a connector for some reason"
        )));
        let mut notifier = ffx_diagnostics::StringNotifier::new();
        let mut check = ConnectSsh::new(&env.context, &m);
        let handle = TargetHandle {
            node_name: Some("test-node".to_string()),
            state: TargetState::Unknown,
            manual: false,
        };
        let res = check.check(handle, &mut notifier).await;
        assert!(res.is_err());
    }

    #[fuchsia::test]
    async fn test_connect_ssh_failures_connector_fails() {
        let env = ffx_config::test_init().await.unwrap();
        // TODO(b/425474866): This should result in a check where we can see that the connection
        // failed multiple times albeit non-fatally.
        let mock_connector = MockConnector::with_results([
            Err(TargetConnectionError::NonFatal(anyhow::anyhow!("Test non-fatal error"))),
            Err(TargetConnectionError::NonFatal(anyhow::anyhow!(
                "Test non-fatal error two: electric boogaloo"
            ))),
            Ok(TargetConnection::FDomain(FDomainConnection::invalid())),
        ]);
        let m = MockSshConnectorProvider::with_res(Ok(mock_connector));
        let mut notifier = ffx_diagnostics::StringNotifier::new();
        let mut check = ConnectSsh::new(&env.context, &m);
        let handle = TargetHandle {
            node_name: Some("test-node".to_string()),
            state: TargetState::Unknown,
            manual: false,
        };
        let res = check.check(handle, &mut notifier).await;
        assert!(res.is_ok());
    }

    fn handle_hwinfo(req: rcs::RemoteControlRequest) {
        let rcs::RemoteControlRequest::ConnectCapability {
            server_channel,
            responder,
            capability_name,
            ..
        } = req
        else {
            panic!("unexpected request: {req:?}");
        };
        let res = if capability_name.contains("hwinfo") {
            let server = fidl::endpoints::ServerEnd::<ProductMarker>::new(server_channel);
            Task::spawn(async move {
                let mut stream = server.into_stream();
                while let Ok(Some(req)) = stream.try_next().await {
                    match req {
                        ProductRequest::GetInfo { responder } => {
                            let res = ProductInfo {
                                name: Some("wubwubwub".to_owned()),
                                ..Default::default()
                            };
                            responder.send(&res).unwrap();
                        }
                    }
                }
            })
            .detach();
            Ok(())
        } else {
            Err(rcs::ConnectCapabilityError::NoMatchingCapabilities)
        };
        responder.send(res).unwrap();
    }

    // Creates a local FDomain client with a namespace that only supports the remote control.
    // This remote control also only supports opening the `hwinfo`
    fn fdomain_remote_control_server(
        handler: impl FnOnce(rcs::RemoteControlRequest) + Send + Copy + 'static,
    ) -> Arc<fdomain_client::Client> {
        fdomain_local::local_client(move || {
            let (client, server) =
                fidl::endpoints::create_endpoints::<fidl_fuchsia_io::DirectoryMarker>();
            Task::spawn(async move {
                let mut stream = server.into_stream();
                while let Ok(Some(req)) = stream.try_next().await {
                    if let fidl_fuchsia_io::DirectoryRequest::Open { path, object, .. } = req {
                        assert_eq!(path, RemoteControlMarker::PROTOCOL_NAME);
                        let server =
                            fidl::endpoints::ServerEnd::<rcs::RemoteControlMarker>::new(object);
                        Task::spawn(async move {
                            let mut stream = server.into_stream();
                            while let Ok(Some(req)) = stream.try_next().await {
                                (handler)(req);
                            }
                        })
                        .detach();
                    } else {
                        panic!("Unexpected request: {req:?}");
                    }
                }
            })
            .detach();
            Ok(client)
        })
    }

    #[fuchsia::test]
    async fn test_connect_remote_control_proxy_success() {
        let mut notifier = ffx_diagnostics::StringNotifier::new();
        let conn = Connection::from_fdomain_client(fdomain_remote_control_server(handle_hwinfo));
        let res = ConnectRemoteControlProxy::new(Duration::from_secs(1))
            .check_with_notifier(conn, &mut notifier)
            .await;
        assert!(res.is_ok(), "Got '{:?}'", res.unwrap_err());
        let (product_info, _) = res.unwrap();
        let output: String = notifier.into();
        assert!(output.contains("SUCCESS"), "Got '{output}'");
        assert_eq!(product_info.name, Some("wubwubwub".to_owned()));
    }

    #[fuchsia::test]
    async fn test_target_identifier() {
        let env = ffx_config::test_env()
            .runtime_config(ffx_config::keys::STATELESS_DEFAULT_TARGET_CONFIGURATION, true)
            .runtime_config(ffx_config::keys::TARGET_DEFAULT_KEY, "foobar")
            .build()
            .await
            .expect("initializing config");
        let mut notifier = ffx_diagnostics::StringNotifier::new();
        let (target, _) = GetTargetSpecifier::new(&env.context)
            .check_with_notifier((), &mut notifier)
            .await
            .expect("running checks");
        if let TargetInfoQuery::NodenameOrSerial(n) = target {
            assert_eq!(n, "foobar");
        } else {
            panic!("Unexpected target: {target:?}")
        };
    }

    #[fuchsia::test]
    async fn test_target_identifier_empty() {
        let env = ffx_config::test_env()
            .runtime_config(ffx_config::keys::STATELESS_DEFAULT_TARGET_CONFIGURATION, true)
            .build()
            .await
            .expect("initializing config");
        let mut notifier = ffx_diagnostics::StringNotifier::new();
        let (target, _) = GetTargetSpecifier::new(&env.context)
            .check_with_notifier((), &mut notifier)
            .await
            .expect("running checks");
        assert!(matches!(target, TargetInfoQuery::First));
    }
}
