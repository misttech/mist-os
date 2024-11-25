// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;
use std::pin::pin;
use std::sync::{Arc, Once};

use assert_matches::assert_matches;
use fidl_fuchsia_net_ext::IntoExt as _;
use fidl_fuchsia_net_interfaces_ext::admin::TerminalError;
use futures::channel::mpsc;
use futures::{StreamExt as _, TryFutureExt as _};
use ip_test_macro::ip_test;
use net_declare::{net_ip_v4, net_ip_v6, net_mac, net_subnet_v4, net_subnet_v6};
use net_types::ethernet::Mac;
use net_types::ip::{AddrSubnetEither, Ip, IpAddr, IpInvariant, Ipv4, Ipv6};
use net_types::{SpecifiedAddr, Witness as _};
use netstack3_core::device::{DeviceId, EthernetLinkDevice};
use netstack3_core::error::AddressResolutionFailed;
use netstack3_core::ip::Ipv6DeviceConfigurationUpdate;
use netstack3_core::neighbor::LinkResolutionResult;
use netstack3_core::routes::{AddableEntry, AddableEntryEither, AddableMetric, Entry, RawMetric};
use {
    fidl_fuchsia_net as fidl_net, fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin,
    fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext,
    fidl_fuchsia_net_neighbor as fnet_neighbor, fidl_fuchsia_net_root as fnet_root,
    fidl_fuchsia_net_routes as fnet_routes, fidl_fuchsia_net_routes_admin as fnet_routes_admin,
    fidl_fuchsia_net_routes_ext as fnet_routes_ext, fidl_fuchsia_netemul_network as net,
    fuchsia_async as fasync,
};

use crate::bindings::ctx::BindingsCtx;
use crate::bindings::devices::{BindingId, Devices};
use crate::bindings::util::{ConversionContext as _, IntoFidl as _, TryIntoFidlWithContext as _};
use crate::bindings::{routes, Ctx, InspectPublisher, LOOPBACK_NAME};

/// Install a logger for tests.
pub(crate) fn set_logger_for_test() {
    struct Logger;

    impl log::Log for Logger {
        fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, record: &log::Record<'_>) {
            println!("[{}] ({}) {}", record.level(), record.target(), record.args())
        }

        fn flush(&self) {}
    }

    static LOGGER_ONCE: Once = Once::new();

    // log::set_logger will panic if called multiple times; using a Once makes
    // set_logger_for_test idempotent
    LOGGER_ONCE.call_once(|| {
        log::set_logger(&Logger).unwrap();
        log::set_max_level(log::LevelFilter::Trace);
    })
}

/// `TestStack` is obtained from [`TestSetupBuilder`] and offers utility methods
/// to connect to the FIDL APIs served by [`TestContext`], as well as keeps
/// track of configured interfaces during the setup procedure.
pub(crate) struct TestStack {
    // Keep a clone of the netstack so we can peek into core contexts and such
    // in tests.
    netstack: crate::bindings::Netstack,
    // The main task running the netstack.
    task: Option<fasync::Task<()>>,
    // A channel sink standing in for ServiceFs when running tests.
    services_sink: mpsc::UnboundedSender<crate::bindings::Service>,
    // The inspector instance given to Netstack, can be used to probe available
    // inspect data.
    inspector: Arc<fuchsia_inspect::Inspector>,
    // Keep track of installed endpoints.
    endpoint_ids: HashMap<String, BindingId>,
}

impl Drop for TestStack {
    fn drop(&mut self) {
        if self.task.is_some() {
            panic!("dropped TestStack without calling shutdown")
        }
    }
}

pub(crate) trait NetstackServiceMarker: fidl::endpoints::DiscoverableProtocolMarker {
    fn make_service(server: fidl::endpoints::ServerEnd<Self>) -> crate::bindings::Service;
}

macro_rules! impl_service_marker {
    ($proto:ty, $svc:ident) => {
        impl_service_marker!($proto, $svc, stream);
    };
    ($proto:ty, $svc:ident, stream) => {
        impl_service_marker!($proto, $svc, |server: fidl::endpoints::ServerEnd<Self>| server
            .into_stream()
            .unwrap());
    };
    ($proto:ty, $svc:ident, server_end) => {
        impl_service_marker!($proto, $svc, |server: fidl::endpoints::ServerEnd<Self>| server);
    };
    ($proto:ty, $svc:ident, $transf:expr) => {
        impl NetstackServiceMarker for $proto {
            fn make_service(server: fidl::endpoints::ServerEnd<Self>) -> crate::bindings::Service {
                let t = $transf;
                crate::bindings::Service::$svc(t(server))
            }
        }
    };
}

impl_service_marker!(fidl_fuchsia_net_debug::DiagnosticsMarker, DebugDiagnostics, server_end);
impl_service_marker!(fidl_fuchsia_net_debug::InterfacesMarker, DebugInterfaces);
impl_service_marker!(fidl_fuchsia_net_interfaces::StateMarker, Interfaces);
impl_service_marker!(fidl_fuchsia_net_interfaces_admin::InstallerMarker, InterfacesAdmin);
impl_service_marker!(fidl_fuchsia_net_neighbor::ViewMarker, Neighbor);
impl_service_marker!(fidl_fuchsia_net_neighbor::ControllerMarker, NeighborController);
impl_service_marker!(fidl_fuchsia_posix_socket_packet::ProviderMarker, PacketSocket);
impl_service_marker!(fidl_fuchsia_posix_socket_raw::ProviderMarker, RawSocket);
impl_service_marker!(fidl_fuchsia_net_root::InterfacesMarker, RootInterfaces);
impl_service_marker!(fidl_fuchsia_net_root::RoutesV4Marker, RootRoutesV4);
impl_service_marker!(fidl_fuchsia_net_root::RoutesV6Marker, RootRoutesV6);
impl_service_marker!(fidl_fuchsia_net_routes::StateMarker, RoutesState);
impl_service_marker!(fidl_fuchsia_net_routes::StateV4Marker, RoutesStateV4);
impl_service_marker!(fidl_fuchsia_net_routes::StateV6Marker, RoutesStateV6);
impl_service_marker!(fidl_fuchsia_posix_socket::ProviderMarker, Socket);
impl_service_marker!(fidl_fuchsia_update_verify::NetstackVerifierMarker, Verifier);
impl_service_marker!(fidl_fuchsia_update_verify::ComponentOtaHealthCheckMarker, HealthCheck);

impl TestStack {
    /// Connects a service to the contained stack.
    pub(crate) fn connect_service(&self, service: crate::bindings::Service) {
        self.services_sink.unbounded_send(service).expect("send service request");
    }

    /// Connect to a discoverable service offered by the netstack.
    pub(crate) fn connect_proxy<M: NetstackServiceMarker>(&self) -> M::Proxy {
        let (proxy, server_end) = fidl::endpoints::create_proxy::<M>().expect("create proxy");
        self.connect_service(M::make_service(server_end));
        proxy
    }

    /// Connects to the `fuchsia.net.interfaces.admin.Installer` service.
    pub(crate) fn connect_interfaces_installer(
        &self,
    ) -> fidl_fuchsia_net_interfaces_admin::InstallerProxy {
        self.connect_proxy::<fidl_fuchsia_net_interfaces_admin::InstallerMarker>()
    }

    /// Gets the [`fnet_interfaces_ext::admin::Control`] for the interface.
    pub(crate) fn get_interface_control(
        &self,
        interface_id: u64,
    ) -> fnet_interfaces_ext::admin::Control {
        let root_interfaces = self.connect_proxy::<fnet_root::InterfacesMarker>();
        let (control, server_end) =
            fidl::endpoints::create_proxy::<fnet_interfaces_admin::ControlMarker>()
                .expect("create proxy");
        root_interfaces.get_admin(interface_id, server_end).expect("get admin failed");
        fnet_interfaces_ext::admin::Control::new(control)
    }

    /// Creates a global route set and authenticates it for the given interface.
    pub(crate) async fn get_global_route_set_with_authenticated_interface<
        I: fnet_routes_ext::admin::FidlRouteAdminIpExt + fnet_routes_ext::FidlRouteIpExt,
    >(
        &self,
        authenticate_nicid: u64,
    ) -> <I::RouteSetMarker as fidl::endpoints::ProtocolMarker>::Proxy
    where
        <I as fidl_fuchsia_net_routes_ext::admin::FidlRouteAdminIpExt>::GlobalRouteTableMarker:
            NetstackServiceMarker,
    {
        let root_routes = self.connect_proxy::<I::GlobalRouteTableMarker>();
        let route_set = fnet_routes_ext::admin::new_global_route_set::<I>(&root_routes)
            .expect("should successfully get route set");

        let control = self.get_interface_control(authenticate_nicid);

        let fnet_interfaces_admin::GrantForInterfaceAuthorization { interface_id, token } =
            control.get_authorization_for_interface().await.expect("should succeed");
        fnet_routes_ext::admin::authenticate_for_interface::<I>(
            &route_set,
            fnet_interfaces_admin::ProofOfInterfaceAuthorization { interface_id, token },
        )
        .await
        .expect("FIDL error while authenticating route set for interface")
        .unwrap_or_else(|e: fnet_routes_admin::AuthenticateForInterfaceError| {
            panic!("error while authenticating route set for interface: {e:?}")
        });

        route_set
    }

    /// Creates a new `fuchsia.net.interfaces/Watcher` for this stack.
    pub(crate) fn new_interfaces_watcher(&self) -> fidl_fuchsia_net_interfaces::WatcherProxy {
        let state = self.connect_proxy::<fidl_fuchsia_net_interfaces::StateMarker>();
        let (watcher, server_end) =
            fidl::endpoints::create_proxy::<fidl_fuchsia_net_interfaces::WatcherMarker>()
                .expect("create proxy");
        state
            .get_watcher(&fidl_fuchsia_net_interfaces::WatcherOptions::default(), server_end)
            .expect("get watcher");
        watcher
    }

    /// Connects to the `fuchsia.posix.socket.Provider` service.
    pub(crate) fn connect_socket_provider(&self) -> fidl_fuchsia_posix_socket::ProviderProxy {
        self.connect_proxy::<fidl_fuchsia_posix_socket::ProviderMarker>()
    }

    /// Waits for interface with given `if_id` to come online.
    pub(crate) async fn wait_for_interface_online(&mut self, if_id: BindingId) {
        let watcher = self.new_interfaces_watcher();
        loop {
            let event = watcher.watch().await.expect("failed to watch");
            let fidl_fuchsia_net_interfaces::Properties { id, online, .. } = match event {
                fidl_fuchsia_net_interfaces::Event::Added(props)
                | fidl_fuchsia_net_interfaces::Event::Changed(props)
                | fidl_fuchsia_net_interfaces::Event::Existing(props) => props,
                fidl_fuchsia_net_interfaces::Event::Idle(fidl_fuchsia_net_interfaces::Empty {}) => {
                    continue;
                }
                fidl_fuchsia_net_interfaces::Event::Removed(id) => {
                    assert_ne!(id, if_id.get(), "interface {} removed while waiting online", if_id);
                    continue;
                }
            };
            if id.expect("missing id") != if_id.get() {
                continue;
            }
            if online.unwrap_or(false) {
                break;
            }
        }
    }

    async fn wait_for_loopback_id(&mut self) -> BindingId {
        let watcher = self.new_interfaces_watcher();
        loop {
            let event = watcher.watch().await.expect("failed to watch");
            let fidl_fuchsia_net_interfaces::Properties { id, name, .. } = match event {
                fidl_fuchsia_net_interfaces::Event::Added(props)
                | fidl_fuchsia_net_interfaces::Event::Existing(props) => props,
                fidl_fuchsia_net_interfaces::Event::Changed(_)
                | fidl_fuchsia_net_interfaces::Event::Idle(fidl_fuchsia_net_interfaces::Empty {})
                | fidl_fuchsia_net_interfaces::Event::Removed(_) => {
                    continue;
                }
            };
            if name.expect("missing name") != LOOPBACK_NAME {
                continue;
            }
            break BindingId::try_from(id.expect("missing id")).expect("bad id");
        }
    }

    pub(crate) async fn remove_interface(&self, id: u64) {
        let control = self.get_interface_control(id);
        control
            .remove()
            .await
            .expect("remove request should be sent successfully")
            .expect("remove request should succeed");
        match control.wait_termination().await {
            TerminalError::Terminal(fnet_interfaces_admin::InterfaceRemovedReason::User) => {}
            TerminalError::Terminal(reason) => {
                panic!("interface control terminated with unexpected reason: {reason:?}")
            }
            TerminalError::Fidl(e) => panic!("observed FIDL error while removing interface: {e:?}"),
        }
    }

    /// Gets an installed interface identifier from the configuration endpoint
    /// `index`.
    pub(crate) fn get_endpoint_id(&self, index: usize) -> BindingId {
        self.get_named_endpoint_id(test_ep_name(index))
    }

    /// Gets an installed interface identifier from the configuration endpoint
    /// `name`.
    pub(crate) fn get_named_endpoint_id(&self, name: impl Into<String>) -> BindingId {
        *self.endpoint_ids.get(&name.into()).unwrap()
    }

    /// Creates a new `TestStack`.
    pub(crate) fn new(
        spy_interface_event_sink: Option<
            mpsc::UnboundedSender<crate::bindings::interfaces_watcher::InterfaceEvent>,
        >,
    ) -> Self {
        let mut seed = crate::bindings::NetstackSeed::default();
        seed.interfaces_worker.run_options.spy_interface_events = spy_interface_event_sink;

        let (services_sink, services) = mpsc::unbounded();
        let inspector = Arc::new(fuchsia_inspect::Inspector::default());
        let netstack = seed.netstack.clone();
        let inspector_cloned = inspector.clone();
        let task = fasync::Task::spawn(async move {
            let inspect_publisher = InspectPublisher::new_for_test(&inspector_cloned);
            seed.serve(services, inspect_publisher).await
        });

        Self {
            netstack,
            task: Some(task),
            services_sink,
            inspector: inspector,
            endpoint_ids: Default::default(),
        }
    }

    /// Helper function to invoke a closure that provides a locked
    /// [`Ctx< BindingsContext>`] provided by this `TestStack`.
    pub(crate) fn with_ctx<R, F: FnOnce(&mut Ctx) -> R>(&mut self, f: F) -> R {
        let mut ctx = self.netstack.ctx.clone();
        f(&mut ctx)
    }

    /// Acquire this `TestStack`'s context.
    pub(crate) fn ctx(&self) -> Ctx {
        self.netstack.ctx.clone()
    }

    /// Acquire this `TestStack`'s netstack.
    pub(crate) fn netstack(&self) -> crate::bindings::Netstack {
        self.netstack.clone()
    }

    /// Gets a reference to the `Inspector` supplied to the `TestStack`.
    pub(crate) fn inspector(&self) -> &fuchsia_inspect::Inspector {
        &self.inspector
    }

    /// Synchronously shutdown the running stack.
    pub(crate) async fn shutdown(mut self) {
        let task = self.task.take().unwrap();

        // Drop all the TestStack, which will release our clone of Ctx and close
        // the services sink, which triggers the main loop shutdown.
        std::mem::drop(self);

        task.await;
    }
}

/// A test setup that than contain multiple stack instances networked together.
pub(crate) struct TestSetup {
    // Let connection to sandbox be made lazily, so a netemul sandbox is not
    // created for tests that don't need it.
    sandbox: Option<netemul::TestSandbox>,
    // Keep around the handle to the virtual networks and endpoints we create to
    // ensure they're not cleaned up before test execution is complete.
    network: Option<net::SetupHandleProxy>,
    stacks: Vec<TestStack>,
}

impl TestSetup {
    /// Gets a mutable reference to the [`TestStack`] at index `i`.
    #[track_caller]
    pub(crate) fn get_mut(&mut self, i: usize) -> &mut TestStack {
        &mut self.stacks[i]
    }

    /// Gets a reference to the [`TestStack`] at index `i`.
    #[track_caller]
    pub(crate) fn get(&self, i: usize) -> &TestStack {
        &self.stacks[i]
    }

    pub(crate) async fn get_endpoint(
        &mut self,
        ep_name: &str,
    ) -> (
        fidl::endpoints::ClientEnd<fidl_fuchsia_hardware_network::DeviceMarker>,
        fidl_fuchsia_hardware_network::PortId,
    ) {
        let epm = self.sandbox().get_endpoint_manager().expect("get endpoint manager");
        let ep = epm
            .get_endpoint(ep_name)
            .await
            .unwrap_or_else(|e| panic!("get endpoint {ep_name}: {e:?}"))
            .unwrap_or_else(|| panic!("failed to retrieve endpoint {ep_name}"))
            .into_proxy()
            .expect("into proxy");

        let (port, server_end) = fidl::endpoints::create_proxy().expect("create proxy");
        ep.get_port(server_end).expect("get port");
        let (device, server_end) = fidl::endpoints::create_endpoints();
        port.get_device(server_end).expect("get device");
        let port_id = port.get_info().await.expect("get port info").id.expect("missing port id");
        (device, port_id)
    }

    /// Creates a new empty `TestSetup`.
    fn new() -> Self {
        set_logger_for_test();
        Self { sandbox: None, network: None, stacks: Vec::new() }
    }

    fn sandbox(&mut self) -> &netemul::TestSandbox {
        self.sandbox
            .get_or_insert_with(|| netemul::TestSandbox::new().expect("create netemul sandbox"))
    }

    async fn configure_network(&mut self, ep_names: impl Iterator<Item = String>) {
        let handle = self
            .sandbox()
            .setup_networks(vec![net::NetworkSetup {
                name: "test_net".to_owned(),
                config: net::NetworkConfig::default(),
                endpoints: ep_names.map(|name| new_endpoint_setup(name)).collect(),
            }])
            .await
            .expect("create network")
            .into_proxy();

        self.network = Some(handle);
    }

    fn add_stack(&mut self, stack: TestStack) {
        self.stacks.push(stack)
    }

    /// Synchronously shut down all the contained stacks.
    pub(crate) async fn shutdown(self) {
        let Self { sandbox, network, stacks } = self;
        // Destroy the sandbox and network, which will cause all devices in the
        // stacks to be removed.
        std::mem::drop(network);
        std::mem::drop(sandbox);
        // Shutdown all the stacks concurrently.
        futures::stream::iter(stacks).for_each_concurrent(None, |stack| stack.shutdown()).await;
    }
}

/// Helper function to retrieve the internal name of an endpoint specified only
/// by an index `i`.
pub(crate) fn test_ep_name(i: usize) -> String {
    format!("test-ep{}", i)
}

fn new_endpoint_setup(name: String) -> net::EndpointSetup {
    net::EndpointSetup { config: None, link_up: true, name }
}

/// A builder structure for [`TestSetup`].
pub(crate) struct TestSetupBuilder {
    endpoints: Vec<String>,
    stacks: Vec<StackSetupBuilder>,
}

impl TestSetupBuilder {
    /// Creates an empty `SetupBuilder`.
    pub(crate) fn new() -> Self {
        Self { endpoints: Vec::new(), stacks: Vec::new() }
    }

    /// Adds an automatically-named endpoint to the setup builder. The automatic
    /// names are taken using [`test_ep_name`] with index starting at 1.
    ///
    /// Multiple calls to `add_endpoint` will result in the creation of multiple
    /// endpoints with sequential indices.
    pub(crate) fn add_endpoint(self) -> Self {
        let id = self.endpoints.len() + 1;
        self.add_named_endpoint(test_ep_name(id))
    }

    /// Ads an endpoint with a given `name`.
    pub(crate) fn add_named_endpoint(mut self, name: impl Into<String>) -> Self {
        self.endpoints.push(name.into());
        self
    }

    /// Adds a stack to create upon building. Stack configuration is provided
    /// by [`StackSetupBuilder`].
    pub(crate) fn add_stack(mut self, stack: StackSetupBuilder) -> Self {
        self.stacks.push(stack);
        self
    }

    /// Adds an empty stack to create upon building. An empty stack contains
    /// no endpoints.
    pub(crate) fn add_empty_stack(mut self) -> Self {
        self.stacks.push(StackSetupBuilder::new());
        self
    }

    /// Attempts to build a [`TestSetup`] with the provided configuration.
    pub(crate) async fn build(self) -> TestSetup {
        let mut setup = TestSetup::new();
        if !self.endpoints.is_empty() {
            setup.configure_network(self.endpoints.into_iter()).await;
        }

        // configure all the stacks:
        for stack_cfg in self.stacks.into_iter() {
            println!("Adding stack: {:?}", stack_cfg);
            let StackSetupBuilder { spy_interface_event_sink, endpoints } = stack_cfg;
            let mut stack = TestStack::new(spy_interface_event_sink);
            let binding_id = stack.wait_for_loopback_id().await;
            assert_eq!(stack.endpoint_ids.insert(LOOPBACK_NAME.to_string(), binding_id), None);

            for (ep_name, addr) in endpoints.into_iter() {
                // get the endpoint from the sandbox config:
                let (endpoint, port_id) = setup.get_endpoint(&ep_name).await;

                let installer = stack.connect_interfaces_installer();

                let (device_control, server_end) =
                    fidl::endpoints::create_proxy().expect("create proxy");
                installer.install_device(endpoint, server_end).expect("install device");

                // Discard strong ownership of device, we're already holding
                // onto the device's netemul definition we don't need to hold on
                // to the netstack side of it too.
                device_control.detach().expect("detach");

                let (interface_control, server_end) =
                    fidl::endpoints::create_proxy().expect("create proxy");
                device_control
                    .create_interface(
                        &port_id,
                        server_end,
                        &fidl_fuchsia_net_interfaces_admin::Options::default(),
                    )
                    .expect("create interface");

                let if_id = interface_control
                    .get_id()
                    .map_ok(|i| BindingId::new(i).expect("nonzero id"))
                    .await
                    .expect("get id");

                // Detach interface_control for the same reason as
                // device_control.
                interface_control.detach().expect("detach");

                assert!(interface_control
                    .enable()
                    .await
                    .expect("calling enable")
                    .expect("enable failed"));

                // We'll ALWAYS await for the newly created interface to come up
                // online before returning, so users of `TestSetupBuilder` can
                // be 100% sure of the state once the setup is done.
                stack.wait_for_interface_online(if_id).await;

                // Disable DAD for simplicity of testing.
                stack.with_ctx(|ctx| {
                    let devices: &Devices<_> = ctx.bindings_ctx().as_ref();
                    let device = devices.get_core_id(if_id).unwrap();
                    let _: Ipv6DeviceConfigurationUpdate = ctx
                        .api()
                        .device_ip::<Ipv6>()
                        .update_configuration(
                            &device,
                            Ipv6DeviceConfigurationUpdate {
                                dad_transmits: Some(None),
                                ..Default::default()
                            },
                        )
                        .unwrap();
                });
                if let Some(addr) = addr {
                    let control = fnet_interfaces_ext::admin::Control::new(interface_control);
                    let (address_state_provider, server_end) = fidl::endpoints::create_proxy::<
                        fnet_interfaces_admin::AddressStateProviderMarker,
                    >()
                    .expect("create proxy should succeed");
                    control
                        .add_address(
                            &addr.into_fidl(),
                            &fnet_interfaces_admin::AddressParameters {
                                add_subnet_route: Some(true),
                                ..Default::default()
                            },
                            server_end,
                        )
                        .expect("add address should succeed");

                    address_state_provider.detach().expect("detach should succeed");
                }
                assert_eq!(stack.endpoint_ids.insert(ep_name, if_id), None);
            }

            setup.add_stack(stack)
        }

        setup
    }
}

/// Helper struct to create stack configuration for [`TestSetupBuilder`].
#[derive(Debug)]
pub(crate) struct StackSetupBuilder {
    endpoints: Vec<(String, Option<AddrSubnetEither>)>,
    spy_interface_event_sink:
        Option<mpsc::UnboundedSender<crate::bindings::interfaces_watcher::InterfaceEvent>>,
}

impl StackSetupBuilder {
    /// Creates a new empty stack (no endpoints) configuration.
    pub(crate) fn new() -> Self {
        Self { endpoints: Vec::new(), spy_interface_event_sink: None }
    }

    /// Adds endpoint number  `index` with optional address configuration
    /// `address` to the builder.
    pub(crate) fn add_endpoint(self, index: usize, address: Option<AddrSubnetEither>) -> Self {
        self.add_named_endpoint(test_ep_name(index), address)
    }

    /// Adds named endpoint `name` with optional address configuration `address`
    /// to the builder.
    pub(crate) fn add_named_endpoint(
        mut self,
        name: impl Into<String>,
        address: Option<AddrSubnetEither>,
    ) -> Self {
        self.endpoints.push((name.into(), address));
        self
    }

    /// Adds a "spy" sink to which
    /// [`crate::bindings::interfaces_watcher::InterfaceEvent`]s will be copied.
    /// Panics if called more than once.
    pub(crate) fn spy_interface_events(
        mut self,
        spy_interface_event_sink: mpsc::UnboundedSender<
            crate::bindings::interfaces_watcher::InterfaceEvent,
        >,
    ) -> Self {
        assert_matches::assert_matches!(
            self.spy_interface_event_sink.replace(spy_interface_event_sink),
            None
        );
        self
    }
}

#[fixture::teardown(TestSetup::shutdown)]
#[fasync::run_singlethreaded(test)]
async fn test_add_device_routes() {
    // create a stack and add a single endpoint to it so we have the interface
    // id:
    let t = TestSetupBuilder::new()
        .add_endpoint()
        .add_stack(StackSetupBuilder::new().add_endpoint(1, None))
        .build()
        .await;

    let test_stack = t.get(0);
    let if_id = test_stack.get_endpoint_id(1);

    let global_route_set =
        test_stack.get_global_route_set_with_authenticated_interface::<Ipv4>(if_id.get()).await;

    let fwd_entry1 = fnet_routes_ext::Route::<Ipv4>::new_forward_with_inherited_metric(
        net_subnet_v4!("192.168.0.0/24"),
        if_id.get(),
        None,
    );
    let fwd_entry2 = fnet_routes_ext::Route::<Ipv4>::new_forward_with_inherited_metric(
        net_subnet_v4!("10.0.0.0/24"),
        if_id.get(),
        None,
    );

    assert!(
        global_route_set
            .add_route(&fwd_entry1.try_into().expect("convert to FIDL should succeed"))
            .await
            .expect("should not get FIDL error")
            .expect("add route should succeed"),
        "route should be newly added"
    );

    assert!(
        global_route_set
            .add_route(&fwd_entry2.try_into().expect("convert to FIDL should succeed"))
            .await
            .expect("should not get FIDL error")
            .expect("add route should succeed"),
        "route should be newly added"
    );

    // finally, check that bad routes will fail:
    // a duplicate entry should return false as it already exists:
    let bad_entry = fwd_entry1.clone();
    assert!(
        !global_route_set
            .add_route(&bad_entry.try_into().expect("convert to FIDL should succeed"))
            .await
            .expect("should not get FIDL error")
            .expect("add route should succeed"),
        "route should already exist"
    );

    // an entry with an invalid subnet should fail with Invalidargs:
    let bad_entry = fnet_routes::RouteV4 {
        destination: fidl_net::Ipv4AddressWithPrefix {
            addr: fidl_net::Ipv4Address { addr: [10, 0, 0, 0] },
            prefix_len: 64,
        },
        action: fnet_routes_ext::RouteAction::Forward(fnet_routes_ext::RouteTarget {
            outbound_interface: if_id.get(),
            next_hop: None,
        })
        .try_into()
        .expect("convert to FIDL RouteAction should succeed"),
        properties: fnet_routes_ext::RouteProperties {
            specified_properties: fnet_routes_ext::SpecifiedRouteProperties {
                metric: fnet_routes::SpecifiedMetric::InheritedFromInterface(fnet_routes::Empty),
            },
        }
        .try_into()
        .expect("convert to FIDL RouteProperties should succeed"),
    };

    assert_eq!(
        global_route_set
            .add_route(&bad_entry)
            .await
            .expect("should not get FIDL error")
            .unwrap_err(),
        fnet_routes_admin::RouteSetError::InvalidDestinationSubnet
    );

    // an entry with a bad device id should fail with PreviouslyAuthenticatedInterfaceNoLongerExists
    // (which can also indicate the interface never existed):
    let bad_entry = fnet_routes_ext::Route::<Ipv4>::new_forward_with_inherited_metric(
        net_subnet_v4!("10.0.0.0/24"),
        10,
        None,
    );

    assert_eq!(
        global_route_set
            .add_route(&bad_entry.try_into().expect("convert to FIDL should succeed"))
            .await
            .expect("should not get FIDL error")
            .unwrap_err(),
        fnet_routes_admin::RouteSetError::PreviouslyAuthenticatedInterfaceNoLongerExists
    );

    t
}

#[fixture::teardown(TestSetup::shutdown)]
#[fasync::run_singlethreaded(test)]
async fn test_list_del_routes() {
    // create a stack and add a single endpoint to it so we have the interface
    // id:
    const EP_NAME: &str = "testep";
    let t = TestSetupBuilder::new()
        .add_named_endpoint(EP_NAME)
        .add_stack(StackSetupBuilder::new().add_named_endpoint(EP_NAME, None))
        .build()
        .await;

    let test_stack = t.get(0);
    let if_id = test_stack.get_named_endpoint_id(EP_NAME);
    let loopback_id = test_stack.get_named_endpoint_id(LOOPBACK_NAME);
    assert_ne!(loopback_id, if_id);
    let device = test_stack.ctx().bindings_ctx().get_core_id(if_id).expect("device exists");
    let sub1 = net_subnet_v4!("192.168.0.0/24");
    let route1: AddableEntryEither<_> = AddableEntry::without_gateway(
        sub1,
        device.downgrade(),
        AddableMetric::ExplicitMetric(RawMetric(0)),
    )
    .into();
    let sub10 = net_subnet_v4!("10.0.0.0/24");
    let route2: AddableEntryEither<_> = AddableEntry::without_gateway(
        sub10,
        device.downgrade(),
        AddableMetric::ExplicitMetric(RawMetric(0)),
    )
    .into();
    let sub10_gateway = SpecifiedAddr::new(net_ip_v4!("10.0.0.1")).unwrap().into();
    let route3: AddableEntryEither<_> = AddableEntry::with_gateway(
        sub10,
        device.downgrade(),
        sub10_gateway,
        AddableMetric::ExplicitMetric(RawMetric(0)),
    )
    .into();

    for route in [route1, route2, route3] {
        test_stack
            .ctx()
            .bindings_ctx()
            .apply_route_change_either(match route.into() {
                netstack3_core::routes::AddableEntryEither::V4(entry) => {
                    routes::ChangeEither::V4(routes::Change::RouteOp(
                        routes::RouteOp::Add(entry),
                        routes::SetMembership::Global,
                    ))
                }
                netstack3_core::routes::AddableEntryEither::V6(entry) => {
                    routes::ChangeEither::V6(routes::Change::RouteOp(
                        routes::RouteOp::Add(entry),
                        routes::SetMembership::Global,
                    ))
                }
            })
            .await
            .map(|outcome| assert_matches!(outcome, routes::ChangeOutcome::Changed))
            .expect("add route should succeed");
    }

    let route1_fwd_entry = fnet_routes_ext::Route::<Ipv4>::new_forward_with_explicit_metric(
        sub1,
        if_id.get(),
        None,
        0,
    );

    let expected_routes_v4 = [
        // route1
        route1_fwd_entry.clone(),
        // route2
        fnet_routes_ext::Route::<Ipv4>::new_forward_with_explicit_metric(
            sub10,
            if_id.get(),
            None,
            0,
        ),
        // route3
        fnet_routes_ext::Route::<Ipv4>::new_forward_with_explicit_metric(
            sub10,
            if_id.get(),
            Some(sub10_gateway),
            0,
        ),
        // Automatically installed routes
        fnet_routes_ext::Route::<Ipv4>::new_forward_with_inherited_metric(
            Ipv4::LOOPBACK_SUBNET,
            loopback_id.get(),
            None,
        ),
        fnet_routes_ext::Route::<Ipv4>::new_forward_with_inherited_metric(
            Ipv4::MULTICAST_SUBNET,
            loopback_id.get(),
            None,
        ),
        fnet_routes_ext::Route::<Ipv4>::new_forward_with_inherited_metric(
            Ipv4::MULTICAST_SUBNET,
            if_id.get(),
            None,
        ),
    ];

    let expected_routes_v6 = [
        fnet_routes_ext::Route::<Ipv6>::new_forward_with_inherited_metric(
            Ipv6::LOOPBACK_SUBNET,
            loopback_id.get(),
            None,
        ),
        fnet_routes_ext::Route::<Ipv6>::new_forward_with_inherited_metric(
            net_subnet_v6!("fe80::/64"),
            if_id.get(),
            None,
        ),
        fnet_routes_ext::Route::<Ipv6>::new_forward_with_inherited_metric(
            Ipv6::MULTICAST_SUBNET,
            loopback_id.get(),
            None,
        ),
        fnet_routes_ext::Route::<Ipv6>::new_forward_with_inherited_metric(
            Ipv6::MULTICAST_SUBNET,
            if_id.get(),
            None,
        ),
    ];

    fn get_routes<I: Ip>(ts: &TestStack) -> Vec<fnet_routes_ext::Route<I>> {
        let mut ctx = ts.ctx();
        let routes: Vec<Entry<<I as Ip>::Addr, DeviceId<BindingsCtx>>> = net_types::map_ip_twice!(
            I,
            IpInvariant(&mut ctx),
            |IpInvariant(ctx)| {
                let mut routes = Vec::new();
                ctx.api()
                    .routes::<I>()
                    .collect_main_table_routes_into::<Entry<<I as Ip>::Addr, DeviceId<BindingsCtx>>, Vec<_>>(
                        &mut routes,
                    );
                routes
            }
        );
        routes
            .into_iter()
            .map(|entry| {
                let route: fnet_routes_ext::Route<I> = entry
                    .try_into_fidl_with_ctx(ctx.bindings_ctx())
                    .expect("failed to map entry into route FIDL");
                route
            })
            .collect::<Vec<_>>()
    }

    assert_eq!(get_routes::<Ipv4>(test_stack), expected_routes_v4);
    assert_eq!(get_routes::<Ipv6>(test_stack), expected_routes_v6);

    // delete route1:
    let global_route_set =
        test_stack.get_global_route_set_with_authenticated_interface::<Ipv4>(if_id.get()).await;
    assert!(global_route_set
        .remove_route(&route1_fwd_entry.try_into().expect("should convert to FIDL successfully"))
        .await
        .expect("should not get FIDL error")
        .expect("remove route should succeed"));

    // Should observe that the route was already removed if we try to delete again:
    assert!(!global_route_set
        .remove_route(&route1_fwd_entry.try_into().expect("should convert to FIDL successfully"))
        .await
        .expect("should not get FIDL error")
        .expect("remove route should succeed"));

    assert_eq!(
        get_routes::<Ipv4>(test_stack),
        expected_routes_v4
            .into_iter()
            .filter(|route| route != &route1_fwd_entry)
            .collect::<Vec<_>>()
    );

    t
}

#[fixture::teardown(TestSetup::shutdown)]
#[fasync::run_singlethreaded(test)]
async fn test_neighbor_table_inspect() {
    const EP_IDX: usize = 1;
    let mut t = TestSetupBuilder::new()
        .add_endpoint()
        .add_stack(StackSetupBuilder::new().add_endpoint(EP_IDX, None))
        .build()
        .await;

    let test_stack = t.get_mut(0);
    let bindings_id = test_stack.get_endpoint_id(EP_IDX);
    test_stack.with_ctx(|ctx| {
        let devices: &Devices<_> = ctx.bindings_ctx().as_ref();
        let device = devices
            .get_core_id(bindings_id)
            .and_then(|d| match d {
                DeviceId::Ethernet(e) => Some(e),
                DeviceId::Loopback(_) | DeviceId::PureIp(_) => None,
            })
            .expect("get_core_id failed");
        let v4_neigh_addr = net_ip_v4!("192.168.0.1");
        let v4_neigh_mac = net_mac!("AA:BB:CC:DD:EE:FF");
        ctx.api()
            .neighbor::<Ipv4, EthernetLinkDevice>()
            .insert_static_entry(&device, v4_neigh_addr, v4_neigh_mac)
            .expect("failed to insert static neighbor entry");
        let v6_neigh_addr = net_ip_v6!("2001:DB8::1");
        let v6_neigh_mac = net_mac!("00:11:22:33:44:55");
        ctx.api()
            .neighbor::<Ipv6, EthernetLinkDevice>()
            .insert_static_entry(&device, v6_neigh_addr, v6_neigh_mac)
            .expect("failed to insert static neighbor entry");
    });
    let inspector = test_stack.inspector();
    use diagnostics_hierarchy::DiagnosticsHierarchyGetter;
    let data = inspector.get_diagnostics_hierarchy();
    println!("{:#?}", data);
    diagnostics_assertions::assert_data_tree!(data, "root": contains {
        "Neighbors": {
            "eth2": {
                "0": {
                    IpAddress: "192.168.0.1",
                    State: "Static",
                    LinkAddress: "AA:BB:CC:DD:EE:FF",
                },
                "1": {
                    IpAddress: "2001:db8::1",
                    State: "Static",
                    LinkAddress: "00:11:22:33:44:55",
                },
            },
        }
    });

    t
}

trait IpExt: Ip + netstack3_core::IpExt {
    type OtherIp: IpExt;
    const ADDR: SpecifiedAddr<Self::Addr>;
    const PREFIX_LEN: u8;
    const FIDL_IP_VERSION: fidl_net::IpVersion;
}

impl IpExt for Ipv4 {
    type OtherIp = Ipv6;
    const ADDR: SpecifiedAddr<Self::Addr> =
        unsafe { SpecifiedAddr::new_unchecked(net_ip_v4!("192.0.2.1")) };
    const PREFIX_LEN: u8 = 24;
    const FIDL_IP_VERSION: fidl_net::IpVersion = fidl_net::IpVersion::V4;
}

impl IpExt for Ipv6 {
    type OtherIp = Ipv4;
    const ADDR: SpecifiedAddr<Self::Addr> =
        unsafe { SpecifiedAddr::new_unchecked(net_ip_v6!("2001:db8::1")) };
    const PREFIX_LEN: u8 = 64;
    const FIDL_IP_VERSION: fidl_net::IpVersion = fidl_net::IpVersion::V6;
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
#[fixture::teardown(TestSetup::shutdown)]
#[ip_test(I)]
#[fasync::run_singlethreaded]
async fn add_remove_neighbor_entry<I: IpExt>() {
    const EP_IDX: usize = 1;
    let t = TestSetupBuilder::new()
        .add_endpoint()
        .add_stack(StackSetupBuilder::new().add_endpoint(EP_IDX, None))
        .build()
        .await;

    let test_stack = t.get(0);
    let bindings_id = test_stack.get_endpoint_id(EP_IDX);
    let mut ctx = test_stack.ctx();
    let devices: &Devices<_> = ctx.bindings_ctx().as_ref();
    let ethernet_device_id = assert_matches!(
        devices.get_core_id(bindings_id).expect("get core id"),
        DeviceId::Ethernet(ethernet_device_id) => ethernet_device_id
    );
    let observer = assert_matches!(
        ctx.api()
           .neighbor::<I, EthernetLinkDevice>()
           .resolve_link_addr(&ethernet_device_id, &I::ADDR),
        LinkResolutionResult::Pending(observer) => observer
    );
    let controller = test_stack.connect_proxy::<fnet_neighbor::ControllerMarker>();
    const MAC: Mac = net_mac!("00:11:22:33:44:55");
    controller
        .add_entry(bindings_id.into(), &IpAddr::from(I::ADDR.get()).into_ext(), &MAC.into_ext())
        .await
        .expect("add_entry FIDL")
        .expect("add_entry");
    assert_eq!(
        observer
            .await
            .expect("address resolution should not be cancelled")
            .expect("address resolution should succeed after adding static entry"),
        MAC,
    );

    controller
        .remove_entry(bindings_id.into(), &IpAddr::from(I::ADDR.get()).into_ext())
        .await
        .expect("remove_entry FIDL")
        .expect("remove_entry");
    assert_matches!(
        ctx.api()
            .neighbor::<I, EthernetLinkDevice>()
            .resolve_link_addr(&ethernet_device_id, &I::ADDR),
        LinkResolutionResult::Pending(_)
    );

    t
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
#[fixture::teardown(TestSetup::shutdown)]
#[ip_test(I)]
#[fasync::run_singlethreaded]
async fn remove_dynamic_neighbor_entry<I: IpExt>() {
    const EP_IDX: usize = 1;
    let t = TestSetupBuilder::new()
        .add_endpoint()
        .add_stack(StackSetupBuilder::new().add_endpoint(
            EP_IDX,
            // NB: Unfortunately AddrSubnetEither cannot be constructed with a
            // const fn on `I`, so it must be constructed here.
            Some(AddrSubnetEither::new(I::ADDR.get().into(), I::PREFIX_LEN).unwrap()),
        ))
        .build()
        .await;

    let test_stack = t.get(0);
    let bindings_id = test_stack.get_endpoint_id(EP_IDX);
    let mut ctx = test_stack.ctx();
    let devices: &Devices<_> = ctx.bindings_ctx().as_ref();
    let ethernet_device_id = assert_matches!(
        devices.get_core_id(bindings_id).expect("get core id"),
        DeviceId::Ethernet(ethernet_device_id) => ethernet_device_id
    );
    let observer = assert_matches!(
        ctx.api()
            .neighbor::<I, EthernetLinkDevice>()
            .resolve_link_addr(&ethernet_device_id, &I::ADDR),
        LinkResolutionResult::Pending(observer) => observer
    );

    let controller = test_stack.connect_proxy::<fnet_neighbor::ControllerMarker>();
    controller
        .remove_entry(bindings_id.into(), &IpAddr::from(I::ADDR.get()).into_ext())
        .await
        .expect("remove_entry FIDL")
        .expect("remove_entry");
    assert_eq!(
        observer.await.expect("observer should not be canceled"),
        Err(AddressResolutionFailed)
    );

    t
}

#[netstack3_core::context_ip_bounds(I, BindingsCtx)]
#[netstack3_core::context_ip_bounds(I::OtherIp, BindingsCtx)]
#[fixture::teardown(TestSetup::shutdown)]
#[ip_test(I)]
#[fasync::run_singlethreaded]
async fn clear_entries<I: IpExt>() {
    const EP_IDX: usize = 1;
    let t = TestSetupBuilder::new()
        .add_endpoint()
        .add_stack(StackSetupBuilder::new().add_endpoint(EP_IDX, None))
        .build()
        .await;

    let test_stack = t.get(0);
    let bindings_id = test_stack.get_endpoint_id(EP_IDX);
    let mut ctx = test_stack.ctx();
    let devices: &Devices<_> = ctx.bindings_ctx().as_ref();
    let ethernet_device_id = assert_matches!(
        devices.get_core_id(bindings_id).expect("get core id"),
        DeviceId::Ethernet(ethernet_device_id) => ethernet_device_id
    );
    let controller = test_stack.connect_proxy::<fnet_neighbor::ControllerMarker>();
    const MAC: Mac = net_mac!("00:11:22:33:44:55");
    for addr in [IpAddr::from(Ipv4::ADDR.get()), IpAddr::from(Ipv6::ADDR.get())] {
        controller
            .add_entry(bindings_id.into(), &addr.into_ext(), &MAC.into_ext())
            .await
            .expect("add_entry FIDL")
            .expect("add_entry");
    }

    controller
        .clear_entries(bindings_id.into(), I::FIDL_IP_VERSION)
        .await
        .expect("clear_entries FIDL")
        .expect("clear_entries");

    assert_matches!(
        ctx.api()
            .neighbor::<I, EthernetLinkDevice>()
            .resolve_link_addr(&ethernet_device_id, &I::ADDR),
        LinkResolutionResult::Pending(_)
    );
    assert_matches!(
        ctx.api()
           .neighbor::<I::OtherIp, EthernetLinkDevice>()
           .resolve_link_addr(&ethernet_device_id, &I::OtherIp::ADDR),
        LinkResolutionResult::Resolved(mac) => assert_eq!(mac, MAC)
    );

    t
}

#[fasync::run_singlethreaded(test)]
async fn device_strong_ids_delay_clean_shutdown() {
    set_logger_for_test();
    let mut t = TestSetupBuilder::new().add_empty_stack().build().await;
    let test_stack = t.get_mut(0);
    let loopback_id = test_stack.wait_for_loopback_id().await;
    let loopback_id = test_stack.ctx().bindings_ctx().devices.get_core_id(loopback_id).unwrap();

    let shutdown = t.shutdown();
    let mut shutdown = pin!(shutdown);
    // Poll shutdown a number of times while yielding to executor to show that
    // shutdown is stuck because we are holding onto a strong loopback id.
    for _ in 0..50 {
        assert_eq!(futures::poll!(&mut shutdown), futures::task::Poll::Pending);
        async_utils::futures::YieldToExecutorOnce::new().await;
    }
    // Now we can finally shutdown.
    std::mem::drop(loopback_id);
    shutdown.await;
}
