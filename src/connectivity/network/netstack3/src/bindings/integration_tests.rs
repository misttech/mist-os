// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::{HashMap, HashSet};
use std::ops::{Deref as _, DerefMut as _};
use std::sync::Once;

use anyhow::{format_err, Context as _, Error};
use assert_matches::assert_matches;
use fidl_fuchsia_net as fidl_net;
use fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin;
use fidl_fuchsia_net_stack as fidl_net_stack;
use fidl_fuchsia_net_stack_ext::FidlReturn as _;
use fidl_fuchsia_netemul_network as net;
use fuchsia_async as fasync;
use futures::FutureExt as _;
use net_types::ip::{Ip, Ipv4, Ipv6, Subnet};
use net_types::{
    ip::{AddrSubnetEither, Ipv4Addr},
    SpecifiedAddr,
};
use netstack3_core::{
    add_ip_addr_subnet,
    device::DeviceId,
    ip::types::{AddableEntry, AddableEntryEither},
    Ctx, NonSyncContext,
};
use packet::Buf;

use crate::bindings::{
    devices::{CommonInfo, DeviceSpecificInfo, EthernetInfo},
    util::{ConversionContext as _, IntoFidl as _, TryFromFidlWithContext as _},
    BindingsNonSyncCtxImpl, RequestStreamExt as _, LOOPBACK_NAME,
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
    endpoint_ids: HashMap<String, u64>,
    // We must keep this sender around to prevent the control task from removing
    // the loopback interface.
    loopback_termination_sender:
        Option<futures::channel::oneshot::Sender<fnet_interfaces_admin::InterfaceRemovedReason>>,
}

struct InterfaceInfo {
    admin_enabled: bool,
    phy_up: bool,
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
    pub(crate) async fn wait_for_interface_online(&mut self, if_id: u64) {
        self.wait_for_interface_online_status(if_id, true).await;
    }

    /// Waits for interface with given `if_id` to go offline.
    pub(crate) async fn wait_for_interface_offline(&mut self, if_id: u64) {
        self.wait_for_interface_online_status(if_id, false).await;
    }

    async fn wait_for_interface_online_status(&mut self, if_id: u64, want_online: bool) {
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
                    assert_ne!(
                        id, if_id,
                        "interface {} removed while waiting online = {}",
                        if_id, want_online
                    );
                    continue;
                }
            };
            if id.expect("missing id") != if_id {
                continue;
            }
            if online.map(|online| online == want_online).unwrap_or(false) {
                break;
            }
        }
    }

    /// Gets an installed interface identifier from the configuration endpoint
    /// `index`.
    pub(crate) fn get_endpoint_id(&self, index: usize) -> u64 {
        self.get_named_endpoint_id(test_ep_name(index))
    }

    /// Gets an installed interface identifier from the configuration endpoint
    /// `name`.
    pub(crate) fn get_named_endpoint_id(&self, name: impl Into<String>) -> u64 {
        *self.endpoint_ids.get(&name.into()).unwrap()
    }

    /// Creates a new `TestStack`.
    pub(crate) fn new() -> Self {
        let ctx = TestContext::new();
        TestStack { ctx, endpoint_ids: HashMap::new(), loopback_termination_sender: None }
    }

    /// Helper function to invoke a closure that provides a locked
    /// [`Ctx< BindingsContext>`] provided by this `TestStack`.
    pub(crate) async fn with_ctx<R, F: FnOnce(&mut Ctx<BindingsNonSyncCtxImpl>) -> R>(
        &mut self,
        f: F,
    ) -> R {
        let mut ctx = self.ctx.netstack.ctx.lock().await;
        f(ctx.deref_mut())
    }

    /// Acquire a lock on this `TestStack`'s context.
    pub(crate) async fn ctx(
        &self,
    ) -> impl std::ops::DerefMut<Target = Ctx<BindingsNonSyncCtxImpl>> + '_ {
        self.ctx.netstack.ctx.lock().await
    }

    async fn get_interface_info(&self, id: u64) -> InterfaceInfo {
        let ctx = self.ctx().await;
        let Ctx { sync_ctx: _, non_sync_ctx } = ctx.deref();
        let device = non_sync_ctx.devices.get_device(id).expect("device");

        let (admin_enabled, phy_up) = assert_matches::assert_matches!(
            device.info(),
            DeviceSpecificInfo::Ethernet(EthernetInfo {
                common_info: CommonInfo {
                    admin_enabled,
                    mtu: _,
                    events: _,
                    name: _,
                    control_hook: _,
                    addresses: _,
                },
                client: _,
                mac: _,
                features: _,
                phy_up,
                interface_control: _,
            }) => (*admin_enabled, *phy_up));
        InterfaceInfo { admin_enabled, phy_up }
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
    pub(crate) fn get(&mut self, i: usize) -> &mut TestStack {
        &mut self.stacks[i]
    }

    /// Acquires a lock on the [`TestContext`] at index `i`.
    pub(crate) async fn ctx(
        &mut self,
        i: usize,
    ) -> impl std::ops::DerefMut<Target = Ctx<BindingsNonSyncCtxImpl>> + '_ {
        self.get(i).ctx.netstack.ctx.lock().await
    }

    async fn get_endpoint(
        &mut self,
        ep_name: &str,
    ) -> Result<fidl::endpoints::ClientEnd<fidl_fuchsia_hardware_ethernet::DeviceMarker>, Error>
    {
        let epm = self.sandbox().get_endpoint_manager()?;
        let ep = match epm.get_endpoint(ep_name).await? {
            Some(ep) => ep.into_proxy()?,
            None => {
                return Err(format_err!("Failed to retrieve endpoint {}", ep_name));
            }
        };

        match ep.get_device().await? {
            fidl_fuchsia_netemul_network::DeviceConnection::Ethernet(e) => Ok(e),
            fidl_fuchsia_netemul_network::DeviceConnection::NetworkDevice(n) => {
                todo!("(48853) Support NetworkDevice for integration tests.  Got unexpected network device {:?}.", n)
            }
        }
    }

    /// Changes a named endpoint `ep_name` link status to `up`.
    pub(crate) async fn set_endpoint_link_up(
        &mut self,
        ep_name: &str,
        up: bool,
    ) -> Result<(), Error> {
        let epm = self.sandbox().get_endpoint_manager()?;
        if let Some(ep) = epm.get_endpoint(ep_name).await? {
            ep.into_proxy()?.set_link_up(up).await?;
            Ok(())
        } else {
            Err(format_err!("Failed to retrieve endpoint {}", ep_name))
        }
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
                config: net::NetworkConfig::EMPTY,
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
                stack.ctx.netstack.add_loopback().await;

            stack.loopback_termination_sender = Some(loopback_termination_sender);
            stack.ctx.tasks.push(loopback_interface_control_task);

            assert_eq!(stack.endpoint_ids.insert(LOOPBACK_NAME.to_string(), binding_id), None);

            for (ep_name, addr) in stack_cfg.endpoints.into_iter() {
                // get the endpoint from the sandbox config:
                let endpoint = setup.get_endpoint(&ep_name).await?;
                let cli = stack.connect_stack()?;
                // add interface:
                let if_id = cli
                    .add_ethernet_interface("fake_topo_path", endpoint)
                    .await
                    .squash_result()
                    .context("Add ethernet interface")?;

                // We'll ALWAYS await for the newly created interface to come up
                // online before returning, so users of `TestSetupBuilder` can
                // be 100% sure of the state once the setup is done.
                stack.wait_for_interface_online(if_id).await;
                if let Some(addr) = addr {
                    stack
                        .with_ctx(|Ctx { sync_ctx, non_sync_ctx }| {
                            let device_info =
                                non_sync_ctx.devices.get_device(if_id).ok_or_else(|| {
                                    format_err!("Failed to get device {} info", if_id)
                                })?;

                            add_ip_addr_subnet(
                                sync_ctx,
                                non_sync_ctx,
                                &device_info.core_id().clone(),
                                addr,
                            )
                            .context("add interface address")
                        })
                        .await?;

                    let (_, subnet) = addr.addr_subnet();

                    let () = cli
                        .add_forwarding_entry(&mut fidl_fuchsia_net_stack::ForwardingEntry {
                            subnet: subnet.into_fidl(),
                            device_id: if_id,
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
    fn add_endpoint(self, index: usize, address: Option<AddrSubnetEither>) -> Self {
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
async fn test_add_remove_interface() {
    let mut t = TestSetupBuilder::new().add_endpoint().add_empty_stack().build().await.unwrap();
    let ep = t.get_endpoint("test-ep1").await.unwrap();
    let test_stack = t.get(0);
    let stack = test_stack.connect_stack().unwrap();
    let if_id = stack
        .add_ethernet_interface("fake_topo_path", ep)
        .await
        .squash_result()
        .expect("Add interface succeeds");

    // remove the interface:
    let () = stack.del_ethernet_interface(if_id).await.squash_result().expect("Remove interface");
    // ensure the interface disappeared from records:
    assert_matches!(
        test_stack.ctx.netstack.ctx.lock().await.non_sync_ctx.devices.get_device(if_id),
        None
    );

    // if we try to remove it again, NotFound should be returned:
    let res =
        stack.del_ethernet_interface(if_id).await.unwrap().expect_err("Failed to remove twice");
    assert_eq!(res, fidl_net_stack::Error::NotFound);
}

#[fasync::run_singlethreaded(test)]
async fn test_ethernet_link_up_down() {
    let mut t = TestSetupBuilder::new()
        .add_endpoint()
        .add_stack(StackSetupBuilder::new().add_endpoint(1, None))
        .build()
        .await
        .unwrap();
    let ep_name = test_ep_name(1);
    let test_stack = t.get(0);
    let if_id = test_stack.get_endpoint_id(1);

    let () = test_stack.wait_for_interface_online(if_id).await;

    // Get the interface info to confirm status indicators are correct.
    let if_info = test_stack.get_interface_info(if_id).await;
    assert!(if_info.phy_up);
    assert!(if_info.admin_enabled);

    // Ensure that the device has been enabled in the core.
    let core_id = {
        let mut ctx = test_stack.ctx().await;
        let core_id = ctx.non_sync_ctx.devices.get_device(if_id).unwrap().core_id().clone();
        check_ip_enabled(ctx.deref_mut(), &core_id, true);
        core_id
    };

    // Setting the link down should disable the interface and disable it from
    // the core. The AdministrativeStatus should remain unchanged.
    assert!(t.set_endpoint_link_up(&ep_name, false).await.is_ok());
    let test_stack = t.get(0);
    test_stack.wait_for_interface_offline(if_id).await;

    // Get the interface info to confirm that it is disabled.
    let if_info = test_stack.get_interface_info(if_id).await;
    assert!(!if_info.phy_up);
    assert!(if_info.admin_enabled);

    // Ensure that the device has been disabled in the core.
    check_ip_enabled(test_stack.ctx().await.deref_mut(), &core_id, false);

    // Setting the link down again should cause no effect on the device state,
    // and should be handled gracefully.
    assert!(t.set_endpoint_link_up(&ep_name, false).await.is_ok());

    // Get the interface info to confirm that it is disabled.
    let test_stack = t.get(0);
    let if_info = test_stack.get_interface_info(if_id).await;
    assert!(!if_info.phy_up);
    assert!(if_info.admin_enabled);

    // Ensure that the device has been disabled in the core.
    check_ip_enabled(test_stack.ctx().await.deref_mut(), &core_id, false);

    // Setting the link up should reenable the interface and enable it in
    // the core.
    assert!(t.set_endpoint_link_up(&ep_name, true).await.is_ok());
    t.get(0).wait_for_interface_online(if_id).await;

    // Get the interface info to confirm that it is reenabled.
    let test_stack = t.get(0);
    let if_info = test_stack.get_interface_info(if_id).await;
    assert!(if_info.phy_up);
    assert!(if_info.admin_enabled);

    // Ensure that the device has been enabled in the core.
    check_ip_enabled(test_stack.ctx().await.deref_mut(), &core_id, true);

    // Setting the link up again should cause no effect on the device state,
    // and should be handled gracefully.
    assert!(t.set_endpoint_link_up(&ep_name, true).await.is_ok());

    // Get the interface info to confirm that there have been no changes.
    let test_stack = t.get(0);
    let if_info = test_stack.get_interface_info(if_id).await;
    assert!(if_info.phy_up);
    assert!(if_info.admin_enabled);

    // Ensure that the device has been enabled in the core.
    let core_id = t
        .get(0)
        .with_ctx(|ctx| {
            let core_id = ctx.non_sync_ctx.devices.get_device(if_id).unwrap().core_id().clone();
            check_ip_enabled(ctx, &core_id, true);
            core_id
        })
        .await;

    // call directly into core to prove that the device was correctly
    // initialized (core will panic if we try to use the device and initialize
    // hasn't been called)
    let mut ctx = t.ctx(0).await;
    let Ctx { sync_ctx, non_sync_ctx } = ctx.deref_mut();
    netstack3_core::device::receive_frame(sync_ctx, non_sync_ctx, &core_id, Buf::new(&mut [], ..))
        .expect("error receiving frame");
}

fn check_ip_enabled<NonSyncCtx: NonSyncContext>(
    Ctx { sync_ctx, non_sync_ctx: _ }: &mut Ctx<NonSyncCtx>,
    core_id: &DeviceId<NonSyncCtx::Instant>,
    expected: bool,
) {
    let ipv4_enabled =
        netstack3_core::device::get_ipv4_configuration(sync_ctx, core_id).ip_config.ip_enabled;
    let ipv6_enabled =
        netstack3_core::device::get_ipv6_configuration(sync_ctx, core_id).ip_config.ip_enabled;

    assert_eq!((ipv4_enabled, ipv6_enabled), (expected, expected));
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
        device_id: if_id,
        next_hop: None,
        metric: 0,
    };
    let mut fwd_entry2 = fidl_net_stack::ForwardingEntry {
        subnet: fidl_net::Subnet {
            addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: [10, 0, 0, 0] }),
            prefix_len: 24,
        },
        device_id: if_id,
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
        device_id: if_id,
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
        device_id: if_id,
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
    let device = test_stack.ctx().await.non_sync_ctx.get_core_id(if_id).expect("device exists");
    let loopback =
        test_stack.ctx().await.non_sync_ctx.get_core_id(loopback_id).expect("device exists");
    let route1_subnet_bytes = [192, 168, 0, 0];
    let route1_subnet_prefix = 24;
    let route1: AddableEntryEither<_> = AddableEntry::without_gateway(
        Subnet::<Ipv4Addr>::new(Ipv4Addr::from(route1_subnet_bytes).into(), route1_subnet_prefix)
            .unwrap(),
        device.clone(),
    )
    .into();
    let sub10 = Subnet::<Ipv4Addr>::new(Ipv4Addr::from([10, 0, 0, 0]).into(), 24).unwrap();
    let route2: AddableEntryEither<_> = AddableEntry::without_gateway(sub10, device.clone()).into();
    let sub10_gateway = SpecifiedAddr::new(Ipv4Addr::from([10, 0, 0, 1])).unwrap().into();
    let route3 = AddableEntry::with_gateway(sub10, None, sub10_gateway).into();

    let () = test_stack
        .with_ctx(|Ctx { sync_ctx, non_sync_ctx }| {
            // add a couple of routes directly into core:
            netstack3_core::add_route(sync_ctx, non_sync_ctx, route1.clone()).unwrap();
            netstack3_core::add_route(sync_ctx, non_sync_ctx, route2.clone()).unwrap();
            netstack3_core::add_route(sync_ctx, non_sync_ctx, route3).unwrap();
        })
        .await;

    let routes = stack.get_forwarding_table().await.expect("Can get forwarding table");
    let route3_with_device: AddableEntryEither<_> =
        AddableEntry::with_gateway(sub10, Some(device.clone()), sub10_gateway).into();

    let auto_routes = [
        // Link local route is added automatically by core.
        AddableEntry::without_gateway(
            net_declare::net_subnet_v6!("fe80::/64").into(),
            device.clone(),
        )
        .into(),
        // Loopback routes are added on netstack creation.
        AddableEntry::without_gateway(Ipv4::LOOPBACK_SUBNET, loopback.clone()).into(),
        AddableEntry::without_gateway(Ipv6::LOOPBACK_SUBNET, loopback.clone()).into(),
        // Multicast routes are added automatically by core.
        AddableEntry::without_gateway(Ipv4::MULTICAST_SUBNET, loopback.clone()).into(),
        AddableEntry::without_gateway(Ipv6::MULTICAST_SUBNET, loopback).into(),
        AddableEntry::without_gateway(Ipv4::MULTICAST_SUBNET, device.clone()).into(),
        AddableEntry::without_gateway(Ipv6::MULTICAST_SUBNET, device.clone()).into(),
    ];
    let got = test_stack
        .with_ctx(|ctx| {
            routes
                .into_iter()
                .map(|e| AddableEntryEither::try_from_fidl_with_ctx(&ctx.non_sync_ctx, e).unwrap())
                .collect::<HashSet<_>>()
        })
        .await;
    let want = HashSet::from_iter(
        [route1, route2.clone(), route3_with_device.clone()].into_iter().chain(auto_routes.clone()),
    );
    assert_eq!(got, want, "got - want = {:?}", got.symmetric_difference(&want));

    // delete route1:
    let mut fwd_entry = fidl_net_stack::ForwardingEntry {
        subnet: fidl_net::Subnet {
            addr: fidl_net::IpAddress::Ipv4(fidl_net::Ipv4Address { addr: route1_subnet_bytes }),
            prefix_len: route1_subnet_prefix,
        },
        device_id: 0,
        next_hop: None,
        metric: 0,
    };
    let () = stack
        .del_forwarding_entry(&mut fwd_entry)
        .await
        .squash_result()
        .expect("can delete device forwarding entry");
    // can't delete again:
    assert_eq!(
        stack.del_forwarding_entry(&mut fwd_entry).await.unwrap().unwrap_err(),
        fidl_net_stack::Error::NotFound
    );

    // check that route was deleted (should've disappeared from core)
    let routes = stack.get_forwarding_table().await.expect("Can get forwarding table");
    assert_eq!(
        test_stack
            .with_ctx(|ctx| {
                routes
                    .into_iter()
                    .map(|e| {
                        AddableEntryEither::try_from_fidl_with_ctx(&ctx.non_sync_ctx, e).unwrap()
                    })
                    .collect::<HashSet<_>>()
            })
            .await,
        HashSet::from_iter([route2, route3_with_device].into_iter().chain(auto_routes))
    );
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
    let device_id = test_stack.get_endpoint_id(1);

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
