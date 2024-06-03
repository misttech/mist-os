// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use fidl_fuchsia_power_broker as fbroker;
use fidl_fuchsia_power_system as fsystem;
use fidl_fuchsia_power_topology_test as fpt;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::ServiceFs;
use futures::StreamExt;
use power_broker_client::{basic_update_fn_factory, run_power_element, PowerElementContext};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use std::{future::Future, pin::Pin};
use tracing::{error, info, warn};

const APPLICATION_ACTIVITY_CONTROLLER: &'static str = "application_activity_controller";

enum IncomingRequest {
    SystemActivityControl(fpt::SystemActivityControlRequestStream),
    TopologyControl(fpt::TopologyControlRequestStream),
}

async fn lease(controller: &PowerElementContext, level: u8) -> Result<fbroker::LeaseControlProxy> {
    let lease_control = controller
        .lessor
        .lease(level)
        .await?
        .map_err(|e| anyhow::anyhow!("{e:?}"))?
        .into_proxy()?;

    Ok(lease_control)
}

struct PowerElement {
    power_element_context: PowerElementContext,
    lease: RefCell<Option<fbroker::LeaseControlProxy>>,
}

impl PowerElement {
    async fn new(
        topology: &fbroker::TopologyProxy,
        element_name: &str,
        valid_levels: &[fbroker::PowerLevel],
        initial_current_level: fbroker::PowerLevel,
        dependencies: Vec<fbroker::LevelDependency>,
        inspect_node: fuchsia_inspect::Node,
    ) -> Result<Rc<Self>> {
        let power_element_context =
            PowerElementContext::builder(topology, element_name, valid_levels)
                .initial_current_level(initial_current_level)
                .dependencies(dependencies)
                .build()
                .await?;

        let this = Rc::new(Self { power_element_context, lease: RefCell::new(None) });
        let this_clone = this.clone();

        fasync::Task::local(async move {
            run_power_element(
                &this_clone.power_element_context.name(),
                &this_clone.power_element_context.required_level,
                initial_current_level,
                Some(inspect_node),
                basic_update_fn_factory(&this_clone.power_element_context),
            )
            .await;
        })
        .detach();

        Ok(this)
    }
}

struct PowerTopology {
    elements: RefCell<HashMap<String, Rc<PowerElement>>>,
}

/// TopologyTestDaemon runs the server for test.power.topology FIDL APIs.
pub struct TopologyTestDaemon {
    inspect_root: fuchsia_inspect::Node,
    topology_proxy: fbroker::TopologyProxy,
    // Holds elements and their leases for test.power.topology.SystemActivityControl.
    system_activity_topology: PowerTopology,
    // Holds elements and their leases for test.power.topology.TopologyControl.
    internal_topology: PowerTopology,
}

impl TopologyTestDaemon {
    pub async fn new(inspect_root: fuchsia_inspect::Node) -> Result<Rc<Self>> {
        let topology_proxy = connect_to_protocol::<fbroker::TopologyMarker>()?;
        let system_activity_topology = PowerTopology { elements: RefCell::new(HashMap::new()) };
        let internal_topology = PowerTopology { elements: RefCell::new(HashMap::new()) };

        Ok(Rc::new(Self {
            inspect_root,
            topology_proxy,
            system_activity_topology,
            internal_topology,
        }))
    }

    pub async fn run(self: Rc<Self>) -> Result<()> {
        info!("Starting FIDL server");
        let mut service_fs = ServiceFs::new_local();

        service_fs
            .dir("svc")
            .add_fidl_service(IncomingRequest::SystemActivityControl)
            .add_fidl_service(IncomingRequest::TopologyControl);
        service_fs
            .take_and_serve_directory_handle()
            .context("failed to serve outgoing namespace")?;

        service_fs
            .for_each_concurrent(None, move |request: IncomingRequest| {
                let ttd = self.clone();
                async move {
                    match request {
                        IncomingRequest::SystemActivityControl(stream) => {
                            fasync::Task::local(ttd.handle_system_activity_request(stream)).detach()
                        }
                        IncomingRequest::TopologyControl(stream) => {
                            fasync::Task::local(ttd.handle_topology_control_request(stream))
                                .detach()
                        }
                    }
                }
            })
            .await;
        Ok(())
    }

    async fn handle_topology_control_request(
        self: Rc<Self>,
        mut stream: fpt::TopologyControlRequestStream,
    ) {
        while let Some(request) = stream.next().await {
            match request {
                Ok(fpt::TopologyControlRequest::Create { responder, elements }) => {
                    let result = responder.send(self.clone().create_topology(elements).await);

                    if let Err(error) = result {
                        warn!(?error, "Error while responding to TopologyControl.Create request");
                    }
                }
                Ok(fpt::TopologyControlRequest::AcquireLease {
                    responder,
                    element_name,
                    level,
                }) => {
                    let result =
                        responder.send(self.clone().acquire_lease(element_name, level).await);

                    if let Err(error) = result {
                        warn!(
                            ?error,
                            "Error while responding to TopologyControl.AcquireLease request"
                        );
                    }
                }
                Ok(fpt::TopologyControlRequest::DropLease { responder, element_name }) => {
                    let result = responder.send(self.clone().drop_lease(element_name).await);

                    if let Err(error) = result {
                        warn!(
                            ?error,
                            "Error while responding to TopologyControl.DropLease request"
                        );
                    }
                }
                Ok(fpt::TopologyControlRequest::_UnknownMethod { ordinal, .. }) => {
                    warn!(?ordinal, "Unknown TopologyControl method");
                }
                Err(error) => {
                    error!(?error, "Error handling TopologyControl request stream");
                }
            }
        }
    }

    async fn handle_system_activity_request(
        self: Rc<Self>,
        mut stream: fpt::SystemActivityControlRequestStream,
    ) {
        while let Some(request) = stream.next().await {
            match request {
                Ok(fpt::SystemActivityControlRequest::StartApplicationActivity { responder }) => {
                    let result = responder.send(self.clone().start_application_activity().await);

                    if let Err(error) = result {
                        warn!(?error, "Error while responding to StartApplicationActivity request");
                    }
                }
                Ok(fpt::SystemActivityControlRequest::StopApplicationActivity { responder }) => {
                    let result = responder.send(self.clone().stop_application_activity().await);

                    if let Err(error) = result {
                        warn!(?error, "Error while responding to StopApplicationActivity request");
                    }
                }
                Ok(fpt::SystemActivityControlRequest::_UnknownMethod { ordinal, .. }) => {
                    warn!(?ordinal, "Unknown ActivityGovernorRequest method");
                }
                Err(error) => {
                    error!(?error, "Error handling SystemActivityControl request stream");
                }
            }
        }
    }

    async fn create_topology(
        self: Rc<Self>,
        mut elements: Vec<fpt::Element>,
    ) -> fpt::TopologyControlCreateResult {
        // Clear old topology when creating a new topology.
        self.internal_topology.elements.borrow_mut().clear();
        while elements.len() > 0 {
            let element = elements.pop().unwrap();
            self.clone().create_element_recursive(element, &mut elements).await?
        }
        Ok(())
    }

    fn create_element_recursive(
        self: Rc<Self>,
        element: fpt::Element,
        elements: &mut Vec<fpt::Element>,
    ) -> Pin<Box<dyn Future<Output = fpt::TopologyControlCreateResult> + '_>> {
        Box::pin(async move {
            let mut dependencies = Vec::new();
            for dependency in element.dependencies {
                let required_element_name = dependency.requires_element;
                // If required_element hasn't been created, find it in `elements` and create it.
                if !self.internal_topology.elements.borrow().contains_key(&required_element_name) {
                    if let Some(index) =
                        elements.iter().position(|e| e.element_name == required_element_name)
                    {
                        let new_element = elements.swap_remove(index);
                        self.clone().create_element_recursive(new_element, elements).await?;
                    } else {
                        return Err(fpt::CreateTopologyGraphError::InvalidTopology);
                    }
                }
                let internal_topology_elements = &self.internal_topology.elements.borrow();
                let power_element_context = &internal_topology_elements
                    .get(&required_element_name)
                    .unwrap()
                    .power_element_context;
                let token = if dependency.dependency_type == fpt::DependencyType::Active {
                    power_element_context.active_dependency_token()
                } else {
                    power_element_context.passive_dependency_token()
                };
                dependencies.push(fbroker::LevelDependency {
                    dependency_type: dependency.dependency_type,
                    dependent_level: dependency.dependent_level,
                    requires_token: token,
                    requires_level: dependency.requires_level,
                });
            }
            let element_name = element.element_name;
            let power_element = PowerElement::new(
                &self.topology_proxy,
                &element_name,
                &element.valid_levels,
                element.initial_current_level,
                dependencies,
                self.inspect_root.create_child(&element_name),
            )
            .await
            .map_err(|err| {
                error!(%err, element_name, "Failed to create power element");
                fpt::CreateTopologyGraphError::Internal
            })?;

            self.internal_topology.elements.borrow_mut().insert(element_name, power_element);
            Ok(())
        })
    }

    async fn acquire_lease(
        self: Rc<Self>,
        element_name: String,
        level: u8,
    ) -> fpt::TopologyControlAcquireLeaseResult {
        let elements = self.internal_topology.elements.borrow_mut();
        let element = elements.get(&element_name).ok_or_else(|| {
            warn!(element_name, "Failed to find element name in the created topology graph");
            fpt::LeaseControlError::InvalidElement
        })?;
        let _ = element.lease.borrow_mut().replace(
            lease(&element.power_element_context, level).await.map_err(|err| {
                warn!(%err, element_name, level, "Failed to acquire a lease");
                fpt::LeaseControlError::Internal
            })?,
        );

        Ok(())
    }

    async fn drop_lease(
        self: Rc<Self>,
        element_name: String,
    ) -> fpt::TopologyControlDropLeaseResult {
        let elements = self.internal_topology.elements.borrow();
        let element = elements.get(&element_name).ok_or_else(|| {
            warn!(element_name, "Failed to find element name in the created topology graph");
            fpt::LeaseControlError::InvalidElement
        })?;
        element.lease.borrow_mut().take();

        Ok(())
    }

    async fn start_application_activity(
        self: Rc<Self>,
    ) -> fpt::SystemActivityControlStartApplicationActivityResult {
        if !self
            .system_activity_topology
            .elements
            .borrow()
            .contains_key(APPLICATION_ACTIVITY_CONTROLLER)
        {
            let sag = connect_to_protocol::<fsystem::ActivityGovernorMarker>().map_err(|err| {
                error!(%err, "Failed to connect to fuchsia.power.system");
                fpt::SystemActivityControlError::Internal
            })?;
            let sag_power_elements = sag.get_power_elements().await.map_err(|err| {
                error!(%err, "Failed to get power elements from SAG");
                fpt::SystemActivityControlError::Internal
            })?;
            let aa_token = sag_power_elements
                .application_activity
                .ok_or_else(|| {
                    error!("Failed to get application_activity power element");
                    fpt::SystemActivityControlError::Internal
                })?
                .active_dependency_token
                .ok_or_else(|| {
                    error!("Failed to get active_dependency_token of application_activity");
                    fpt::SystemActivityControlError::Internal
                })?;
            let aa_controller = PowerElement::new(
                &self.topology_proxy,
                APPLICATION_ACTIVITY_CONTROLLER,
                &[0, 1],
                0,
                vec![fbroker::LevelDependency {
                    dependency_type: fbroker::DependencyType::Active,
                    dependent_level: 1,
                    requires_token: aa_token,
                    requires_level: 1,
                }],
                self.inspect_root.create_child(APPLICATION_ACTIVITY_CONTROLLER),
            )
            .await
            .map_err(|err| {
                error!(%err, "Failed to create application activity controller");
                fpt::SystemActivityControlError::Internal
            })?;
            self.system_activity_topology
                .elements
                .borrow_mut()
                .insert(APPLICATION_ACTIVITY_CONTROLLER.to_string(), aa_controller);
        }
        let elements = self.system_activity_topology.elements.borrow_mut();
        let aa_controller_element = elements.get(APPLICATION_ACTIVITY_CONTROLLER).unwrap();
        if aa_controller_element.lease.borrow().is_none() {
            let _ = aa_controller_element.lease.borrow_mut().insert(
                lease(&aa_controller_element.power_element_context, 1).await.map_err(|err| {
                    error!(%err, "Failed to require a lease for application activity controller");
                    fpt::SystemActivityControlError::Internal
                })?,
            );
        }

        Ok(())
    }

    async fn stop_application_activity(
        self: Rc<Self>,
    ) -> fpt::SystemActivityControlStartApplicationActivityResult {
        self.system_activity_topology
            .elements
            .borrow_mut()
            .get(APPLICATION_ACTIVITY_CONTROLLER)
            .and_then(|a| a.lease.borrow_mut().take());

        Ok(())
    }
}
