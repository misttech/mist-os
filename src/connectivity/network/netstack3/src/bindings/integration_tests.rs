// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;
use std::sync::Once;

use anyhow::{format_err, Context as _, Error};
use fidl_fuchsia_net as fidl_net;
use fidl_fuchsia_net_ext::IntoExt;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_stack as fidl_net_stack;
use fidl_fuchsia_net_stack_ext::FidlReturn as _;
use fidl_fuchsia_netemul_network as net;
use fuchsia_async as fasync;
use futures::{FutureExt as _, StreamExt as _};
use net_declare::{net_ip_v4, net_subnet_v4, net_subnet_v6};
use net_types::{
    ip::{AddrSubnetEither, Ip, IpAddress, Ipv4, Ipv6},
    SpecifiedAddr,
};
use netstack3_core::{
    add_ip_addr_subnet,
    device::update_ipv6_configuration,
    ip::{
        device::slaac::STABLE_IID_SECRET_KEY_BYTES,
        types::{AddableEntry, AddableEntryEither, AddableMetric, RawMetric},
    },
};

use crate::bindings::{
    devices::{BindingId, Devices},
    util::{ConversionContext as _, IntoFidl as _, TryIntoFidlWithContext as _},
    Ctx, RequestStreamExt as _, DEFAULT_INTERFACE_METRIC, LOOPBACK_NAME,
};

mod util {
    use fuchsia_async as fasync;
    use netstack3_core::sync::Mutex;
    use std::sync::Arc;

    /// A thread-safe collection of [`fasync::Task`].
    #[derive(Clone, Default)]
    pub(super) struct TaskCollection(Arc<Mutex<Vec<fasync::Task<()>>>>);

    impl TaskCollection {
        /// Creates a new `TaskCollection` with the tasks in `i`.
        pub(super) fn new(i: impl Iterator<Item = fasync::Task<()>>) -> Self {
            Self(Arc::new(Mutex::new(i.collect())))
        }

        /// Pushes `task` into the collection.
        ///
        /// Hiding this in a method ensures that the task lock can't be
        /// interleaved with executor yields.
        pub(super) fn push(&self, task: fasync::Task<()>) {
            let Self(t) = self;
            t.lock().push(task);
        }
    }
}

/// log::Log implementation that uses stdout.
///
/// Useful when debugging tests.
struct Logger;

impl log::Log for Logger {
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        println!("[{}] ({}) {}", record.level(), record.module_path().unwrap_or(""), record.args())
    }

    fn flush(&self) {}
}

static LOGGER: Logger = Logger;

static LOGGER_ONCE: Once = Once::new();

/// Install a logger for tests.
pub(crate) fn set_logger_for_test() {
    // log::set_logger will panic if called multiple times; using a Once makes
    // set_logger_for_test idempotent
    LOGGER_ONCE.call_once(|| {
        log::set_logger(&LOGGER).unwrap();
        log::set_max_level(log::LevelFilter::Trace);
    })
}

#[derive(Clone)]
/// A netstack context for testing.
pub(crate) struct TestContext {
    netstack: crate::bindings::Netstack,
    interfaces_watcher_sink: crate::bindings::interfaces_watcher::WorkerWatcherSink,
    tasks: util::TaskCollection,
}

impl TestContext {
    fn new() -> Self {
        let crate::bindings::NetstackSeed { netstack, interfaces_worker, interfaces_watcher_sink } =
            crate::bindings::NetstackSeed::default();

        Self {
            netstack,
            interfaces_watcher_sink,
            tasks: util::TaskCollection::new(std::iter::once(fasync::Task::spawn(
                interfaces_worker.run().map(|r| {
                    let _: futures::stream::FuturesUnordered<_> = r.expect("watcher failed");
                }),
            ))),
        }
    }
}

/// A holder for a [`TestContext`].
/// `TestStack` is obtained from [`TestSetupBuilder`] and offers utility methods
/// to connect to the FIDL APIs served by [`TestContext`], as well as keeps
/// track of configured interfaces during the setup procedure.
pub(crate) struct TestStack {
    ctx: TestContext,
    endpoint_ids: HashMap<String, BindingId>,
    // We must keep this sender around to prevent the control task from removing
    // the loopback interface.
    loopback_termination_sender:
        Option<futures::channel::oneshot::Sender<fnet_interfaces_admin::InterfaceRemovedReason>>,
}

impl TestStack {
    /// Connects to the `fuchsia.net.stack.Stack` service.
    pub(crate) fn connect_stack(&self) -> Result<fidl_fuchsia_net_stack::StackProxy, Error> {
        let (stack, rs) =
            fidl::endpoints::create_proxy_and_stream::<fidl_fuchsia_net_stack::StackMarker>()?;
        fasync::Task::spawn(rs.serve_with(|rs| {
            crate::bindings::stack_fidl_worker::StackFidlWorker::serve(
                self.ctx.netstack.clone(),
                rs,
            )
        }))
        .detach();
        Ok(stack)
    }

    /// Connects to the `fuchsia.net.interfaces.admin.Installer` service.
    pub(crate) fn connect_interfaces_installer(
        &self,
    ) -> fidl_fuchsia_net_interfaces_admin::InstallerProxy {
        let (installer, rs) = fidl::endpoints::create_proxy_and_stream::<
            fidl_fuchsia_net_interfaces_admin::InstallerMarker,
        >()
        .expect("create endpoints");
        let task_stream = crate::bindings::interfaces_admin::serve(self.ctx.netstack.clone(), rs);
        let task_sink = self.ctx.tasks.clone();
        self.ctx.tasks.push(fasync::Task::spawn(task_stream.for_each(move |r| {
            futures::future::ready(task_sink.push(r.expect("error serving interfaces installer")))
        })));
        installer
    }

    /// Creates a new `fuchsia.net.interfaces/Watcher` for this stack.
    pub(crate) async fn new_interfaces_watcher(&self) -> fidl_fuchsia_net_interfaces::WatcherProxy {
        let (watcher, rs) = fidl::endpoints::create_proxy_and_stream::<
            fidl_fuchsia_net_interfaces::WatcherMarker,
        >()
        .expect("create proxy");
        let mut sender = self.ctx.interfaces_watcher_sink.clone();
        sender.add_watcher(rs).await.expect("add watcher");
        watcher
    }

    /// Connects to the `fuchsia.posix.socket.Provider` service.
    pub(crate) fn connect_socket_provider(
        &self,
    ) -> Result<fidl_fuchsia_posix_socket::ProviderProxy, Error> {
        let (stack, rs) = fidl::endpoints::create_proxy_and_stream::<
            fidl_fuchsia_posix_socket::ProviderMarker,
        >()?;
        fasync::Task::spawn(
            rs.serve_with(|rs| crate::bindings::socket::serve(self.ctx.netstack.ctx.clone(), rs)),
        )
        .detach();
        Ok(stack)
    }

    /// Waits for interface with given `if_id` to come online.
    pub(crate) async fn wait_for_interface_online(&mut self, if_id: BindingId) {
        let watcher = self.new_interfaces_watcher().await;
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
        let ctx = TestContext::new();
        TestStack { ctx, endpoint_ids: HashMap::new(), loopback_termination_sender: None }
    }

    /// Helper function to invoke a closure that provides a locked
    /// [`Ctx< BindingsContext>`] provided by this `TestStack`.
    pub(crate) fn with_ctx<R, F: FnOnce(&mut Ctx) -> R>(&mut self, f: F) -> R {
        let mut ctx = self.ctx.netstack.ctx.clone();
        f(&mut ctx)
    }

    /// Acquire this `TestStack`'s context.
    pub(crate) fn ctx(&self) -> Ctx {
        self.ctx.netstack.ctx.clone()
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
    ) -> Result<
        (
            fidl::endpoints::ClientEnd<fidl_fuchsia_hardware_network::DeviceMarker>,
            fidl_fuchsia_hardware_network::PortId,
        ),
        Error,
    > {
        let epm = self.sandbox().get_endpoint_manager()?;
        let ep = match epm.get_endpoint(ep_name).await? {
            Some(ep) => ep.into_proxy()?,
            None => {
                return Err(format_err!("Failed to retrieve endpoint {}", ep_name));
            }
        };

        let (port, server_end) = fidl::endpoints::create_proxy().context("create proxy")?;
        ep.get_port(server_end).context("get port")?;
        let (device, server_end) = fidl::endpoints::create_endpoints();
        port.get_device(server_end).context("get device")?;
        let port_id = port
            .get_info()
            .await
            .context("get port info")?
            .id
            .ok_or_else(|| anyhow::anyhow!("missing port id"))?;
        Ok((device, port_id))
    }

    /// Creates a new empty `TestSetup`.
    fn new() -> Result<Self, Error> {
        set_logger_for_test();
        Ok(Self { sandbox: None, _network: None, stacks: Vec::new() })
    }

    fn sandbox(&mut self) -> &netemul::TestSandbox {
        self.sandbox
            .get_or_insert_with(|| netemul::TestSandbox::new().expect("create netemul sandbox"))
    }

    async fn configure_network(
        &mut self,
        ep_names: impl Iterator<Item = String>,
    ) -> Result<(), Error> {
        let handle = self
            .sandbox()
            .setup_networks(vec![net::NetworkSetup {
                name: "test_net".to_owned(),
                config: net::NetworkConfig::default(),
                endpoints: ep_names.map(|name| new_endpoint_setup(name)).collect(),
            }])
            .await
            .context("create network")?
            .into_proxy();

        self._network = Some(handle);
        Ok(())
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
    pub(crate) async fn build(self) -> Result<TestSetup, Error> {
        let mut setup = TestSetup::new()?;
        if !self.endpoints.is_empty() {
            let () = setup.configure_network(self.endpoints.into_iter()).await?;
        }

        // configure all the stacks:
        for stack_cfg in self.stacks.into_iter() {
            println!("Adding stack: {:?}", stack_cfg);
            let mut stack = TestStack::new();
            let (loopback_termination_sender, binding_id, loopback_interface_control_task) =
                stack.ctx.netstack.add_loopback();

            stack.loopback_termination_sender = Some(loopback_termination_sender);
            stack.ctx.tasks.push(loopback_interface_control_task);

            assert_eq!(stack.endpoint_ids.insert(LOOPBACK_NAME.to_string(), binding_id), None);

            for (ep_name, addr) in stack_cfg.endpoints.into_iter() {
                // get the endpoint from the sandbox config:
                let (endpoint, mut port_id) = setup.get_endpoint(&ep_name).await?;

                let installer = stack.connect_interfaces_installer();

                let (device_control, server_end) =
                    fidl::endpoints::create_proxy().context("create proxy")?;
                installer.install_device(endpoint, server_end).context("install device")?;

                // Discard strong ownership of device, we're already holding
                // onto the device's netemul definition we don't need to hold on
                // to the netstack side of it too.
                device_control.detach().context("detach")?;

                let (interface_control, server_end) =
                    fidl::endpoints::create_proxy().context("create proxy")?;
                device_control
                    .create_interface(
                        &mut port_id,
                        server_end,
                        &fidl_fuchsia_net_interfaces_admin::Options::default(),
                    )
                    .context("create interface")?;

                let if_id = interface_control
                    .get_id()
                    .await
                    .context("get id")
                    .and_then(|i| BindingId::new(i).ok_or(Error::msg("id is 0")))?;

                // Detach interface_control for the same reason as
                // device_control.
                interface_control.detach().context("detach")?;

                assert!(interface_control
                    .enable()
                    .await
                    .context("calling enable")?
                    .map_err(|e| format_err!("enable error {:?}", e))?);

                // We'll ALWAYS await for the newly created interface to come up
                // online before returning, so users of `TestSetupBuilder` can
                // be 100% sure of the state once the setup is done.
                stack.wait_for_interface_online(if_id).await;

                // Disable DAD for simplicity of testing.
                stack.with_ctx(|Ctx { sync_ctx, non_sync_ctx }| {
                    let devices: &Devices<_> = non_sync_ctx.as_ref();
                    let device = devices.get_core_id(if_id).unwrap();
                    update_ipv6_configuration(sync_ctx, non_sync_ctx, &device, |config| {
                        config.dad_transmits = None
                    })
                    .unwrap()
                });
                if let Some(addr) = addr {
                    stack.with_ctx(|Ctx { sync_ctx, non_sync_ctx }| {
                        let core_id = non_sync_ctx
                            .devices
                            .get_core_id(if_id)
                            .ok_or_else(|| format_err!("Failed to get device {} info", if_id))?;

                        add_ip_addr_subnet(sync_ctx, non_sync_ctx, &core_id, addr)
                            .context("add interface address")
                    })?;

                    let (_, subnet) = addr.addr_subnet();

                    let stack = stack.connect_stack().context("connect stack")?;
                    stack
                        .add_forwarding_entry(&mut fidl_fuchsia_net_stack::ForwardingEntry {
                            subnet: subnet.into_fidl(),
                            device_id: if_id.get(),
                            next_hop: None,
                            metric: 0,
                        })
                        .await
                        .squash_result()
                        .context("add forwarding entry")?;
                }
                assert_eq!(stack.endpoint_ids.insert(ep_name, if_id), None);
            }

            setup.add_stack(stack)
        }

        Ok(setup)
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
        .await
        .unwrap();
    let test_stack = t.get(0);
    let stack = test_stack.connect_stack().unwrap();
    let if_id = test_stack.get_endpoint_id(1);

    let mut fwd_entry1 = fidl_net_stack::ForwardingEntry {
        subnet: fidl_net::Subnet {
            addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [192, 168, 0, 0] }),
            prefix_len: 24,
        },
        device_id: if_id.get(),
        next_hop: None,
        metric: 0,
    };
    let mut fwd_entry2 = fidl_net_stack::ForwardingEntry {
        subnet: fidl_net::Subnet {
            addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [10, 0, 0, 0] }),
            prefix_len: 24,
        },
        device_id: if_id.get(),
        next_hop: None,
        metric: 0,
    };

    let () = stack
        .add_forwarding_entry(&mut fwd_entry1)
        .await
        .squash_result()
        .expect("Add forwarding entry succeeds");
    let () = stack
        .add_forwarding_entry(&mut fwd_entry2)
        .await
        .squash_result()
        .expect("Add forwarding entry succeeds");

    // finally, check that bad routes will fail:
    // a duplicate entry should fail with AlreadyExists:
    let mut bad_entry = fidl_net_stack::ForwardingEntry {
        subnet: fidl_net::Subnet {
            addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [192, 168, 0, 0] }),
            prefix_len: 24,
        },
        device_id: if_id.get(),
        next_hop: None,
        metric: 0,
    };
    assert_eq!(
        stack.add_forwarding_entry(&mut bad_entry).await.unwrap().unwrap_err(),
        fidl_net_stack::Error::AlreadyExists
    );
    // an entry with an invalid subnet should fail with Invalidargs:
    let mut bad_entry = fidl_net_stack::ForwardingEntry {
        subnet: fidl_net::Subnet {
            addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [10, 0, 0, 0] }),
            prefix_len: 64,
        },
        device_id: if_id.get(),
        next_hop: None,
        metric: 0,
    };
    assert_eq!(
        stack.add_forwarding_entry(&mut bad_entry).await.unwrap().unwrap_err(),
        fidl_net_stack::Error::InvalidArgs
    );
    // an entry with a bad devidce id should fail with NotFound:
    let mut bad_entry = fidl_net_stack::ForwardingEntry {
        subnet: fidl_net::Subnet {
            addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [10, 0, 0, 0] }),
            prefix_len: 24,
        },
        device_id: 10,
        next_hop: None,
        metric: 0,
    };
    assert_eq!(
        stack.add_forwarding_entry(&mut bad_entry).await.unwrap().unwrap_err(),
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
        .await
        .unwrap();

    let test_stack = t.get(0);
    let stack = test_stack.connect_stack().unwrap();
    let if_id = test_stack.get_named_endpoint_id(EP_NAME);
    let loopback_id = test_stack.get_named_endpoint_id(LOOPBACK_NAME);
    assert_ne!(loopback_id, if_id);
    let device = test_stack.ctx().non_sync_ctx.get_core_id(if_id).expect("device exists");
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

    let () = test_stack.with_ctx(|Ctx { sync_ctx, non_sync_ctx }| {
        // add a couple of routes directly into core:
        netstack3_core::add_route(sync_ctx, non_sync_ctx, route1).unwrap();
        netstack3_core::add_route(sync_ctx, non_sync_ctx, route2).unwrap();
        netstack3_core::add_route(sync_ctx, non_sync_ctx, route3).unwrap();
    });

    let mut route1_fwd_entry = fidl_net_stack::ForwardingEntry {
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
        let Ctx { sync_ctx, non_sync_ctx } = &mut ctx;
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
        .del_forwarding_entry(&mut route1_fwd_entry)
        .await
        .squash_result()
        .expect("can delete device forwarding entry");
    // can't delete again:
    assert_eq!(
        stack.del_forwarding_entry(&mut route1_fwd_entry).await.unwrap().unwrap_err(),
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
        .await
        .unwrap();

    let test_stack = t.get(0);
    let stack = test_stack.connect_stack().unwrap();
    let device_id = test_stack.get_endpoint_id(1).get();

    let subnet = fidl_net::Subnet {
        addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [192, 168, 0, 0] }),
        prefix_len: 24,
    };
    let mut fwd_entry = fidl_net_stack::ForwardingEntry {
        subnet,
        device_id: 0,
        next_hop: Some(Box::new(fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address {
            addr: [192, 168, 0, 1],
        }))),
        metric: 0,
    };

    // Cannot add gateway route without device set or on-link route to gateway.
    assert_eq!(
        stack.add_forwarding_entry(&mut fwd_entry).await.unwrap(),
        Err(fidl_net_stack::Error::BadState)
    );
    let mut device_fwd_entry = fidl_net_stack::ForwardingEntry {
        subnet: fwd_entry.subnet,
        device_id,
        next_hop: None,
        metric: 0,
    };
    let () = stack
        .add_forwarding_entry(&mut device_fwd_entry)
        .await
        .squash_result()
        .expect("add device route");

    let () =
        stack.add_forwarding_entry(&mut fwd_entry).await.squash_result().expect("add device route");

    // finally, check that bad routes will fail:
    // a duplicate entry should fail with AlreadyExists:
    assert_eq!(
        stack.add_forwarding_entry(&mut fwd_entry).await.unwrap(),
        Err(fidl_net_stack::Error::AlreadyExists)
    );
}

fn get_slaac_secret<'s>(
    test_stack: &'s mut TestStack,
    if_id: BindingId,
) -> Option<[u8; STABLE_IID_SECRET_KEY_BYTES]> {
    test_stack.with_ctx(|Ctx { sync_ctx, non_sync_ctx }| {
        let device = AsRef::<Devices<_>>::as_ref(non_sync_ctx).get_core_id(if_id).unwrap();
        netstack3_core::device::update_ipv6_configuration(
            sync_ctx,
            non_sync_ctx,
            &device,
            |config| {
                config.slaac_config.temporary_address_configuration.map(|t| t.secret_key.clone())
            },
        )
        .unwrap()
    })
}

#[fasync::run_singlethreaded(test)]
async fn test_ipv6_slaac_secret_stable() {
    const ENDPOINT: &'static str = "endpoint";

    let mut t = TestSetupBuilder::new()
        .add_named_endpoint(ENDPOINT)
        .add_empty_stack()
        .build()
        .await
        .unwrap();
    let (endpoint, mut port_id) = t.get_endpoint(ENDPOINT).await.expect("has endpoint");

    let test_stack = t.get(0);
    let installer = test_stack.connect_interfaces_installer();
    let (device_control, server_end) = fidl::endpoints::create_proxy().expect("new proxy");
    installer.install_device(endpoint, server_end).expect("install device");

    let (interface_control, server_end) = fidl::endpoints::create_proxy().unwrap();
    device_control
        .create_interface(
            &mut port_id,
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
