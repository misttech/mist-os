// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;
use std::sync::{Arc, Once};

use fidl_fuchsia_net as fidl_net;
use fidl_fuchsia_net_ext::IntoExt;
use fidl_fuchsia_net_stack as fidl_net_stack;
use fidl_fuchsia_net_stack_ext::FidlReturn as _;
use fidl_fuchsia_netemul_network as net;
use fuchsia_async as fasync;
use futures::{channel::mpsc, TryFutureExt as _};
use net_declare::{net_ip_v4, net_subnet_v4, net_subnet_v6};
use net_types::{
    ip::{AddrSubnetEither, Ip, IpAddress, Ipv4, Ipv6},
    SpecifiedAddr,
};
use netstack3_core::{
    add_ip_addr_subnet,
    device::update_ipv6_configuration,
    ip::{
        device::{slaac::STABLE_IID_SECRET_KEY_BYTES, Ipv6DeviceConfigurationUpdate},
        types::{AddableEntry, AddableEntryEither, AddableMetric, RawMetric},
    },
};
use tracing::Subscriber;
use tracing_subscriber::{
    fmt::{
        format::{self, FormatEvent, FormatFields},
        FmtContext,
    },
    registry::LookupSpan,
};

use crate::bindings::{
    devices::{BindingId, Devices},
    util::{ConversionContext as _, IntoFidl as _, TryIntoFidlWithContext as _},
    Ctx, DEFAULT_INTERFACE_METRIC, LOOPBACK_NAME,
};

struct LogFormatter;

impl<S, N> FormatEvent<S, N> for LogFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        write!(
            writer,
            "[{}] ({}) ",
            event.metadata().level(),
            event.metadata().module_path().unwrap_or("")
        )?;
        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

static LOGGER_ONCE: Once = Once::new();

/// Install a logger for tests.
pub(crate) fn set_logger_for_test() {
    // `init` will panic if called multiple times; using a Once makes
    // set_logger_for_test idempotent
    LOGGER_ONCE.call_once(|| {
        tracing_subscriber::fmt()
            .event_format(LogFormatter)
            .with_max_level(tracing::Level::TRACE)
            .init();
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
    _task: fasync::Task<()>,
    // A channel sink standing in for ServiceFs when running tests.
    services_sink: mpsc::UnboundedSender<crate::bindings::Service>,
    // The inspector instance given to Netstack, can be used to probe available
    // inspect data.
    _inspector: Arc<fuchsia_inspect::Inspector>,
    // Keep track of installed endpoints.
    endpoint_ids: HashMap<String, BindingId>,
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
impl_service_marker!(fidl_fuchsia_net_filter::FilterMarker, Filter);
impl_service_marker!(fidl_fuchsia_net_interfaces::StateMarker, Interfaces);
impl_service_marker!(fidl_fuchsia_net_interfaces_admin::InstallerMarker, InterfacesAdmin);
impl_service_marker!(fidl_fuchsia_net_neighbor::ViewMarker, Neighbor);
impl_service_marker!(fidl_fuchsia_posix_socket_packet::ProviderMarker, PacketSocket);
impl_service_marker!(fidl_fuchsia_posix_socket_raw::ProviderMarker, RawSocket);
impl_service_marker!(fidl_fuchsia_net_root::InterfacesMarker, RootInterfaces);
impl_service_marker!(fidl_fuchsia_net_routes::StateMarker, RoutesState);
impl_service_marker!(fidl_fuchsia_net_routes::StateV4Marker, RoutesStateV4);
impl_service_marker!(fidl_fuchsia_net_routes::StateV6Marker, RoutesStateV6);
impl_service_marker!(fidl_fuchsia_posix_socket::ProviderMarker, Socket);
impl_service_marker!(fidl_fuchsia_net_stack::StackMarker, Stack);
impl_service_marker!(fidl_fuchsia_update_verify::NetstackVerifierMarker, Verifier);

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

    /// Connects to the `fuchsia.net.stack.Stack` service.
    pub(crate) fn connect_stack(&self) -> fidl_fuchsia_net_stack::StackProxy {
        self.connect_proxy::<fidl_fuchsia_net_stack::StackMarker>()
    }

    /// Connects to the `fuchsia.net.interfaces.admin.Installer` service.
    pub(crate) fn connect_interfaces_installer(
        &self,
    ) -> fidl_fuchsia_net_interfaces_admin::InstallerProxy {
        self.connect_proxy::<fidl_fuchsia_net_interfaces_admin::InstallerMarker>()
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
    pub(crate) fn new() -> Self {
        let seed = crate::bindings::NetstackSeed::default();
        let (services_sink, services) = mpsc::unbounded();
        let inspector = Arc::new(fuchsia_inspect::Inspector::default());
        let netstack = seed.netstack.clone();
        let inspector_cloned = inspector.clone();
        let task =
            fasync::Task::spawn(async move { seed.serve(services, &inspector_cloned).await });

        Self {
            netstack,
            _task: task,
            services_sink,
            _inspector: inspector,
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
}

/// A test setup that than contain multiple stack instances networked together.
pub(crate) struct TestSetup {
    // Let connection to sandbox be made lazily, so a netemul sandbox is not
    // created for tests that don't need it.
    sandbox: Option<netemul::TestSandbox>,
    // Keep around the handle to the virtual networks and endpoints we create to
    // ensure they're not cleaned up before test execution is complete.
    _network: Option<net::SetupHandleProxy>,
    stacks: Vec<TestStack>,
}

impl TestSetup {
    /// Gets the [`TestStack`] at index `i`.
    #[track_caller]
    pub(crate) fn get(&mut self, i: usize) -> &mut TestStack {
        &mut self.stacks[i]
    }

    async fn get_endpoint(
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
        Self { sandbox: None, _network: None, stacks: Vec::new() }
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

        self._network = Some(handle);
    }

    fn add_stack(&mut self, stack: TestStack) {
        self.stacks.push(stack)
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
            let mut stack = TestStack::new();
            let binding_id = stack.wait_for_loopback_id().await;
            assert_eq!(stack.endpoint_ids.insert(LOOPBACK_NAME.to_string(), binding_id), None);

            for (ep_name, addr) in stack_cfg.endpoints.into_iter() {
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
                    let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
                    let devices: &Devices<_> = non_sync_ctx.as_ref();
                    let device = devices.get_core_id(if_id).unwrap();
                    let _: Ipv6DeviceConfigurationUpdate = update_ipv6_configuration(
                        sync_ctx,
                        non_sync_ctx,
                        &device,
                        Ipv6DeviceConfigurationUpdate {
                            dad_transmits: Some(None),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                });
                if let Some(addr) = addr {
                    stack.with_ctx(|ctx| {
                        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
                        let core_id = non_sync_ctx
                            .devices
                            .get_core_id(if_id)
                            .unwrap_or_else(|| panic!("failed to get device {if_id} info"));

                        add_ip_addr_subnet(sync_ctx, non_sync_ctx, &core_id, addr)
                            .expect("add interface address")
                    });

                    let (_, subnet) = addr.addr_subnet();

                    let stack = stack.connect_stack();
                    stack
                        .add_forwarding_entry(&fidl_fuchsia_net_stack::ForwardingEntry {
                            subnet: subnet.into_fidl(),
                            device_id: if_id.get(),
                            next_hop: None,
                            metric: 0,
                        })
                        .await
                        .squash_result()
                        .expect("add forwarding entry");
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
pub struct StackSetupBuilder {
    endpoints: Vec<(String, Option<AddrSubnetEither>)>,
}

impl StackSetupBuilder {
    /// Creates a new empty stack (no endpoints) configuration.
    pub(crate) fn new() -> Self {
        Self { endpoints: Vec::new() }
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
}

#[fasync::run_singlethreaded(test)]
async fn test_add_device_routes() {
    // create a stack and add a single endpoint to it so we have the interface
    // id:
    let mut t = TestSetupBuilder::new()
        .add_endpoint()
        .add_stack(StackSetupBuilder::new().add_endpoint(1, None))
        .build()
        .await;

    let test_stack = t.get(0);
    let stack = test_stack.connect_stack();
    let if_id = test_stack.get_endpoint_id(1);

    let fwd_entry1 = fidl_net_stack::ForwardingEntry {
        subnet: fidl_net::Subnet {
            addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [192, 168, 0, 0] }),
            prefix_len: 24,
        },
        device_id: if_id.get(),
        next_hop: None,
        metric: 0,
    };
    let fwd_entry2 = fidl_net_stack::ForwardingEntry {
        subnet: fidl_net::Subnet {
            addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [10, 0, 0, 0] }),
            prefix_len: 24,
        },
        device_id: if_id.get(),
        next_hop: None,
        metric: 0,
    };

    let () = stack
        .add_forwarding_entry(&fwd_entry1)
        .await
        .squash_result()
        .expect("Add forwarding entry succeeds");
    let () = stack
        .add_forwarding_entry(&fwd_entry2)
        .await
        .squash_result()
        .expect("Add forwarding entry succeeds");

    // finally, check that bad routes will fail:
    // a duplicate entry should fail with AlreadyExists:
    let bad_entry = fidl_net_stack::ForwardingEntry {
        subnet: fidl_net::Subnet {
            addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [192, 168, 0, 0] }),
            prefix_len: 24,
        },
        device_id: if_id.get(),
        next_hop: None,
        metric: 0,
    };
    assert_eq!(
        stack.add_forwarding_entry(&bad_entry).await.unwrap().unwrap_err(),
        fidl_net_stack::Error::AlreadyExists
    );
    // an entry with an invalid subnet should fail with Invalidargs:
    let bad_entry = fidl_net_stack::ForwardingEntry {
        subnet: fidl_net::Subnet {
            addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [10, 0, 0, 0] }),
            prefix_len: 64,
        },
        device_id: if_id.get(),
        next_hop: None,
        metric: 0,
    };
    assert_eq!(
        stack.add_forwarding_entry(&bad_entry).await.unwrap().unwrap_err(),
        fidl_net_stack::Error::InvalidArgs
    );
    // an entry with a bad devidce id should fail with NotFound:
    let bad_entry = fidl_net_stack::ForwardingEntry {
        subnet: fidl_net::Subnet {
            addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [10, 0, 0, 0] }),
            prefix_len: 24,
        },
        device_id: 10,
        next_hop: None,
        metric: 0,
    };
    assert_eq!(
        stack.add_forwarding_entry(&bad_entry).await.unwrap().unwrap_err(),
        fidl_net_stack::Error::NotFound
    );
}

#[fasync::run_singlethreaded(test)]
async fn test_list_del_routes() {
    // create a stack and add a single endpoint to it so we have the interface
    // id:
    const EP_NAME: &str = "testep";
    let mut t = TestSetupBuilder::new()
        .add_named_endpoint(EP_NAME)
        .add_stack(StackSetupBuilder::new().add_named_endpoint(EP_NAME, None))
        .build()
        .await;

    let test_stack = t.get(0);
    let stack = test_stack.connect_stack();
    let if_id = test_stack.get_named_endpoint_id(EP_NAME);
    let loopback_id = test_stack.get_named_endpoint_id(LOOPBACK_NAME);
    assert_ne!(loopback_id, if_id);
    let device = test_stack.ctx().non_sync_ctx().get_core_id(if_id).expect("device exists");
    let sub1 = net_subnet_v4!("192.168.0.0/24");
    let route1: AddableEntryEither<_> = AddableEntry::without_gateway(
        sub1,
        device.clone(),
        AddableMetric::ExplicitMetric(RawMetric(0)),
    )
    .into();
    let sub10 = net_subnet_v4!("10.0.0.0/24");
    let route2: AddableEntryEither<_> = AddableEntry::without_gateway(
        sub10,
        device.clone(),
        AddableMetric::ExplicitMetric(RawMetric(0)),
    )
    .into();
    let sub10_gateway = SpecifiedAddr::new(net_ip_v4!("10.0.0.1")).unwrap().into();
    let route3 = AddableEntry::with_gateway(
        sub10,
        None,
        sub10_gateway,
        AddableMetric::ExplicitMetric(RawMetric(0)),
    )
    .into();

    let () = test_stack.with_ctx(|ctx| {
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        // add a couple of routes directly into core:
        netstack3_core::add_route(sync_ctx, non_sync_ctx, route1).unwrap();
        netstack3_core::add_route(sync_ctx, non_sync_ctx, route2).unwrap();
        netstack3_core::add_route(sync_ctx, non_sync_ctx, route3).unwrap();
    });

    let route1_fwd_entry = fidl_net_stack::ForwardingEntry {
        subnet: sub1.into_ext(),
        device_id: if_id.get(),
        next_hop: None,
        metric: 0,
    };

    let expected_routes = [
        // Automatically installed routes
        fidl_net_stack::ForwardingEntry {
            subnet: crate::bindings::IPV4_LIMITED_BROADCAST_SUBNET.into_ext(),
            device_id: loopback_id.get(),
            next_hop: None,
            metric: crate::bindings::DEFAULT_LOW_PRIORITY_METRIC,
        },
        fidl_net_stack::ForwardingEntry {
            subnet: crate::bindings::IPV4_LIMITED_BROADCAST_SUBNET.into_ext(),
            device_id: if_id.get(),
            next_hop: None,
            metric: crate::bindings::DEFAULT_LOW_PRIORITY_METRIC,
        },
        // route1
        route1_fwd_entry.clone(),
        // route2
        fidl_net_stack::ForwardingEntry {
            subnet: sub10.into_ext(),
            device_id: if_id.get(),
            next_hop: None,
            metric: 0,
        },
        // route3
        fidl_net_stack::ForwardingEntry {
            subnet: sub10.into_ext(),
            device_id: if_id.get(),
            next_hop: Some(Box::new(sub10_gateway.to_ip_addr().into_ext())),
            metric: 0,
        },
        // More automatically installed routes
        fidl_net_stack::ForwardingEntry {
            subnet: Ipv4::LOOPBACK_SUBNET.into_ext(),
            device_id: loopback_id.get(),
            next_hop: None,
            metric: DEFAULT_INTERFACE_METRIC,
        },
        fidl_net_stack::ForwardingEntry {
            subnet: Ipv4::MULTICAST_SUBNET.into_ext(),
            device_id: loopback_id.get(),
            next_hop: None,
            metric: DEFAULT_INTERFACE_METRIC,
        },
        fidl_net_stack::ForwardingEntry {
            subnet: Ipv4::MULTICAST_SUBNET.into_ext(),
            device_id: if_id.get(),
            next_hop: None,
            metric: DEFAULT_INTERFACE_METRIC,
        },
        fidl_net_stack::ForwardingEntry {
            subnet: Ipv6::LOOPBACK_SUBNET.into_ext(),
            device_id: loopback_id.get(),
            next_hop: None,
            metric: DEFAULT_INTERFACE_METRIC,
        },
        fidl_net_stack::ForwardingEntry {
            subnet: net_subnet_v6!("fe80::/64").into_ext(),
            device_id: if_id.get(),
            next_hop: None,
            metric: DEFAULT_INTERFACE_METRIC,
        },
        fidl_net_stack::ForwardingEntry {
            subnet: Ipv6::MULTICAST_SUBNET.into_ext(),
            device_id: loopback_id.get(),
            next_hop: None,
            metric: DEFAULT_INTERFACE_METRIC,
        },
        fidl_net_stack::ForwardingEntry {
            subnet: Ipv6::MULTICAST_SUBNET.into_ext(),
            device_id: if_id.get(),
            next_hop: None,
            metric: DEFAULT_INTERFACE_METRIC,
        },
    ];

    fn get_routing_table(ts: &TestStack) -> Vec<fidl_net_stack::ForwardingEntry> {
        let mut ctx = ts.ctx();
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        netstack3_core::ip::get_all_routes(sync_ctx)
            .into_iter()
            .map(|entry| {
                entry
                    .try_into_fidl_with_ctx(&non_sync_ctx)
                    .expect("failed to map forwarding entry into FIDL")
            })
            .collect()
    }

    let routes = get_routing_table(test_stack);
    assert_eq!(routes, expected_routes);

    // delete route1:
    let () = stack
        .del_forwarding_entry(&route1_fwd_entry)
        .await
        .squash_result()
        .expect("can delete device forwarding entry");
    // can't delete again:
    assert_eq!(
        stack.del_forwarding_entry(&route1_fwd_entry).await.unwrap().unwrap_err(),
        fidl_net_stack::Error::NotFound
    );

    // check that route was deleted (should've disappeared from core)
    let routes = get_routing_table(test_stack);
    let expected_routes =
        expected_routes.into_iter().filter(|route| route != &route1_fwd_entry).collect::<Vec<_>>();
    assert_eq!(routes, expected_routes);
}

#[fasync::run_singlethreaded(test)]
async fn test_add_remote_routes() {
    let mut t = TestSetupBuilder::new()
        .add_endpoint()
        .add_stack(StackSetupBuilder::new().add_endpoint(1, None))
        .build()
        .await;

    let test_stack = t.get(0);
    let stack = test_stack.connect_stack();
    let device_id = test_stack.get_endpoint_id(1).get();

    let subnet = fidl_net::Subnet {
        addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [192, 168, 0, 0] }),
        prefix_len: 24,
    };
    let fwd_entry = fidl_net_stack::ForwardingEntry {
        subnet,
        device_id: 0,
        next_hop: Some(Box::new(fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address {
            addr: [192, 168, 0, 1],
        }))),
        metric: 0,
    };

    // Cannot add gateway route without device set or on-link route to gateway.
    assert_eq!(
        stack.add_forwarding_entry(&fwd_entry).await.unwrap(),
        Err(fidl_net_stack::Error::BadState)
    );
    let device_fwd_entry = fidl_net_stack::ForwardingEntry {
        subnet: fwd_entry.subnet,
        device_id,
        next_hop: None,
        metric: 0,
    };
    let () = stack
        .add_forwarding_entry(&device_fwd_entry)
        .await
        .squash_result()
        .expect("add device route");

    let () =
        stack.add_forwarding_entry(&fwd_entry).await.squash_result().expect("add device route");

    // finally, check that bad routes will fail:
    // a duplicate entry should fail with AlreadyExists:
    assert_eq!(
        stack.add_forwarding_entry(&fwd_entry).await.unwrap(),
        Err(fidl_net_stack::Error::AlreadyExists)
    );
}

fn get_slaac_secret<'s>(
    test_stack: &'s mut TestStack,
    if_id: BindingId,
) -> Option<[u8; STABLE_IID_SECRET_KEY_BYTES]> {
    test_stack.with_ctx(|ctx| {
        let (sync_ctx, non_sync_ctx) = ctx.contexts_mut();
        let device = AsRef::<Devices<_>>::as_ref(non_sync_ctx).get_core_id(if_id).unwrap();
        netstack3_core::device::get_ipv6_configuration_and_flags(sync_ctx, &device)
            .config
            .slaac_config
            .temporary_address_configuration
            .map(|t| t.secret_key)
    })
}

#[fasync::run_singlethreaded(test)]
async fn test_ipv6_slaac_secret_stable() {
    const ENDPOINT: &'static str = "endpoint";

    let mut t =
        TestSetupBuilder::new().add_named_endpoint(ENDPOINT).add_empty_stack().build().await;

    let (endpoint, port_id) = t.get_endpoint(ENDPOINT).await;

    let test_stack = t.get(0);
    let installer = test_stack.connect_interfaces_installer();
    let (device_control, server_end) = fidl::endpoints::create_proxy().expect("new proxy");
    installer.install_device(endpoint, server_end).expect("install device");

    let (interface_control, server_end) = fidl::endpoints::create_proxy().unwrap();
    device_control
        .create_interface(
            &port_id,
            server_end,
            &fidl_fuchsia_net_interfaces_admin::Options::default(),
        )
        .expect("create interface");

    let if_id = BindingId::new(interface_control.get_id().await.unwrap()).unwrap();
    let installed_secret =
        get_slaac_secret(test_stack, if_id).expect("has temporary address secret");

    // Bringing the interface up does not change the secret.
    assert_eq!(true, interface_control.enable().await.expect("FIDL call").expect("enabled"));
    let enabled_secret = get_slaac_secret(test_stack, if_id).expect("has temporary address secret");
    assert_eq!(enabled_secret, installed_secret);

    // Bringing the interface down and up does not change the secret.
    assert_eq!(true, interface_control.disable().await.expect("FIDL call").expect("disabled"));
    assert_eq!(true, interface_control.enable().await.expect("FIDL call").expect("enabled"));

    assert_eq!(get_slaac_secret(test_stack, if_id), Some(enabled_secret));
}
