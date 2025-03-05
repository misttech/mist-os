// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Error, Result};
use fidl::endpoints::{ClientEnd, Proxy, ServerEnd};
use fidl_test_powerelementrunner::ControlMarker;
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at_dir_root};
use fuchsia_component::server::ServiceFs;
use futures::StreamExt;
use log::{error, info, warn};
use power_broker_client::PowerElementContext;
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use zx::{HandleBased, Rights};
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_power_broker as fbroker, fidl_fuchsia_power_topology_test as fpt,
    fuchsia_async as fasync,
};

const ELEMENTS_COLLECTION: &'static str = "elements";

enum IncomingRequest {
    TopologyControl(fpt::TopologyControlRequestStream),
}

async fn lease(lessor: &fbroker::LessorProxy, level: u8) -> Result<fbroker::LeaseControlProxy> {
    let lease_control =
        lessor.lease(level).await?.map_err(|e| anyhow::anyhow!("{e:?}"))?.into_proxy();

    Ok(lease_control)
}

/// This struct does not wrap a PowerElementContext because the required_level and current_level
/// proxies need to be converted to client ends. This struct temporarily stores the client ends
/// after the power element is created. Then, after the realm builder finishes adding all the child
/// components, the client ends are taken and sent to the designated component for the power element
/// to run on.
struct PowerElement {
    element_control: fbroker::ElementControlProxy,
    lessor: fbroker::LessorProxy,
    required_level: RefCell<Option<ClientEnd<fbroker::RequiredLevelMarker>>>,
    current_level: RefCell<Option<ClientEnd<fbroker::CurrentLevelMarker>>>,
    assertive_dependency_token: fbroker::DependencyToken,
    opportunistic_dependency_token: fbroker::DependencyToken,
    initial_level: fbroker::PowerLevel,
    lease: RefCell<Option<fbroker::LeaseControlProxy>>,
    component_controller: fcomponent::ControllerProxy,
}

impl PowerElement {
    async fn new(
        realm: &fcomponent::RealmProxy,
        topology: &fbroker::TopologyProxy,
        element_name: &str,
        valid_levels: &[fbroker::PowerLevel],
        initial_current_level: fbroker::PowerLevel,
        dependencies: Vec<fbroker::LevelDependency>,
    ) -> Result<Self> {
        let power_element_context =
            PowerElementContext::builder(topology, element_name, valid_levels)
                .initial_current_level(initial_current_level)
                .dependencies(dependencies)
                .build()
                .await?;

        let assertive_dependency_token =
            power_element_context.assertive_dependency_token().expect("token not registered");
        let opportunistic_dependency_token =
            power_element_context.opportunistic_dependency_token().expect("token not registered");

        // Destructure PowerElementContext and convert current_level and required_level proxies to
        // client ends. `into_client_end` will only succeed if there are no active clones of the
        // proxy.
        let PowerElementContext { element_control, lessor, required_level, current_level, .. } =
            power_element_context;

        let (component_controller, controller_server_end) = fidl::endpoints::create_proxy();
        let _ = realm
            .create_child(
                &fdecl::CollectionRef { name: ELEMENTS_COLLECTION.into() },
                &fdecl::Child {
                    name: Some(element_name.to_string()),
                    url: Some("#meta/power-element-runner.cm".to_string()),
                    startup: Some(fdecl::StartupMode::Eager),
                    ..Default::default()
                },
                fcomponent::CreateChildArgs {
                    controller: Some(controller_server_end),
                    ..Default::default()
                },
            )
            .await?;

        Ok(Self {
            element_control,
            lessor,
            required_level: RefCell::new(Some(required_level.into_client_end().unwrap())),
            current_level: RefCell::new(Some(current_level.into_client_end().unwrap())),
            assertive_dependency_token,
            opportunistic_dependency_token,
            initial_level: initial_current_level,
            lease: RefCell::new(None),
            component_controller,
        })
    }
}

struct PowerTopology {
    elements: RefCell<HashMap<String, PowerElement>>,
}

impl PowerTopology {
    async fn run_power_elements(&self) -> Result<(), Error> {
        let realm = connect_to_protocol::<fcomponent::RealmMarker>()
            .map_err(|err| anyhow!("Failed to run power elements, no realm connection {}", err))?;

        for (element_name, element) in self.elements.borrow().iter() {
            let current_level = element
                .current_level
                .borrow_mut()
                .take()
                .ok_or_else(|| anyhow!("Element ({element_name}) not added"))?;
            let required_level = element
                .required_level
                .borrow_mut()
                .take()
                .ok_or_else(|| anyhow!("Element ({element_name}) not added"))?;
            let initial_current_level = element.initial_level;

            let (element_exposed_dir, element_exposed_dir_server) = fidl::endpoints::create_proxy();
            if let Err(e) = realm
                .open_exposed_dir(
                    &fidl_fuchsia_component_decl::ChildRef {
                        name: element_name.clone(),
                        collection: Some(ELEMENTS_COLLECTION.to_string()),
                    },
                    element_exposed_dir_server,
                )
                .await
            {
                return Err(anyhow!("Failed to run power element: {}, error {}", element_name, e));
            }

            let proxy = connect_to_protocol_at_dir_root::<ControlMarker>(&element_exposed_dir)?;

            if let Err(_) = proxy
                .start(&element_name, initial_current_level, required_level, current_level)
                .await?
            {
                error!(element_name:%; "Failed to run power element");
                return Err(anyhow!("Failed to run power element: {}", element_name));
            }
        }
        Ok(())
    }
}

/// TopologyTestDaemon runs the server for test.power.topology FIDL APIs.
pub struct TopologyTestDaemon {
    topology_proxy: fbroker::TopologyProxy,
    // Holds elements and their leases for test.power.topology.TopologyControl.
    internal_topology: PowerTopology,
}

impl TopologyTestDaemon {
    pub async fn new() -> Result<Rc<Self>> {
        let topology_proxy = connect_to_protocol::<fbroker::TopologyMarker>()?;
        let internal_topology = PowerTopology { elements: RefCell::new(HashMap::new()) };

        Ok(Rc::new(Self { topology_proxy, internal_topology }))
    }

    pub async fn run(self: Rc<Self>) -> Result<()> {
        info!("Starting FIDL server");
        let mut service_fs = ServiceFs::new_local();

        service_fs.dir("svc").add_fidl_service(IncomingRequest::TopologyControl);
        service_fs
            .take_and_serve_directory_handle()
            .context("failed to serve outgoing namespace")?;

        service_fs
            .for_each_concurrent(None, move |request: IncomingRequest| {
                let ttd = self.clone();
                async move {
                    match request {
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
                        warn!(error:?; "Error while responding to TopologyControl.Create request");
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
                            error:?;
                            "Error while responding to TopologyControl.AcquireLease request"
                        );
                    }
                }
                Ok(fpt::TopologyControlRequest::DropLease { responder, element_name }) => {
                    let result = responder.send(self.clone().drop_lease(element_name).await);

                    if let Err(error) = result {
                        warn!(
                            error:?;
                            "Error while responding to TopologyControl.DropLease request"
                        );
                    }
                }
                Ok(
                    fidl_fuchsia_power_topology_test::TopologyControlRequest::OpenStatusChannel {
                        responder,
                        element_name,
                        status_channel,
                    },
                ) => {
                    let result = responder
                        .send(self.clone().open_status_channel(element_name, status_channel).await);
                    if let Err(error) = result {
                        warn!(
                            error:?;
                            "Error while responding to TopologyControl.OpenStatusChannel request"
                        );
                    }
                }
                Ok(fpt::TopologyControlRequest::_UnknownMethod { ordinal, .. }) => {
                    warn!(ordinal:?; "Unknown TopologyControl method");
                }
                Err(error) => {
                    error!(error:?; "Error handling TopologyControl request stream");
                }
            }
        }
    }

    async fn create_topology(
        self: Rc<Self>,
        mut elements: Vec<fpt::Element>,
    ) -> fpt::TopologyControlCreateResult {
        // Clear old topology when creating a new topology.
        for (_, element) in self.internal_topology.elements.borrow().iter() {
            let _ = element
                .component_controller
                .destroy()
                .await
                .expect("Failed to destroy old element instance");
        }
        self.internal_topology.elements.borrow_mut().clear();

        let realm = connect_to_protocol::<fcomponent::RealmMarker>().map_err(|err| {
            error!(err:%; "Failed to connect to fuchsia.component.Realm");
            fpt::CreateTopologyGraphError::Internal
        })?;

        while elements.len() > 0 {
            let element = elements.pop().unwrap();
            self.clone().create_element_recursive(&realm, element, &mut elements).await?
        }

        self.internal_topology.run_power_elements().await.map_err(|err| {
            error!(err:%; "Failed to run power elements on separate components");
            fpt::CreateTopologyGraphError::Internal
        })?;

        Ok(())
    }

    fn create_element_recursive<'a>(
        self: Rc<Self>,
        realm: &'a fcomponent::RealmProxy,
        element: fpt::Element,
        elements: &'a mut Vec<fpt::Element>,
    ) -> Pin<Box<dyn Future<Output = fpt::TopologyControlCreateResult> + 'a>> {
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
                        self.clone().create_element_recursive(realm, new_element, elements).await?;
                    } else {
                        return Err(fpt::CreateTopologyGraphError::InvalidTopology);
                    }
                }
                let internal_topology_elements = &self.internal_topology.elements.borrow();
                let power_element =
                    &internal_topology_elements.get(&required_element_name).unwrap();
                let token = if dependency.dependency_type == fpt::DependencyType::Assertive {
                    power_element
                        .assertive_dependency_token
                        .duplicate_handle(Rights::SAME_RIGHTS)
                        .expect("failed to duplicate token")
                } else {
                    power_element
                        .opportunistic_dependency_token
                        .duplicate_handle(Rights::SAME_RIGHTS)
                        .expect("failed to duplicate token")
                };
                dependencies.push(fbroker::LevelDependency {
                    dependency_type: dependency.dependency_type,
                    dependent_level: dependency.dependent_level,
                    requires_token: token,
                    requires_level_by_preference: vec![dependency.requires_level],
                });
            }
            let element_name = element.element_name;
            let power_element = PowerElement::new(
                realm,
                &self.topology_proxy,
                &element_name,
                &element.valid_levels,
                element.initial_current_level,
                dependencies,
            )
            .await
            .map_err(|err| {
                error!(err:%, element_name:%; "Failed to create power element");
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
            warn!(element_name:%; "Failed to find element name in the created topology graph");
            fpt::LeaseControlError::InvalidElement
        })?;
        let _ = element.lease.borrow_mut().replace(lease(&element.lessor, level).await.map_err(
            |err| {
                warn!(err:%, element_name:%, level; "Failed to acquire a lease");
                fpt::LeaseControlError::Internal
            },
        )?);

        Ok(())
    }

    async fn drop_lease(
        self: Rc<Self>,
        element_name: String,
    ) -> fpt::TopologyControlDropLeaseResult {
        let elements = self.internal_topology.elements.borrow();
        let element = elements.get(&element_name).ok_or_else(|| {
            warn!(element_name:%; "Failed to find element name in the created topology graph");
            fpt::LeaseControlError::InvalidElement
        })?;
        element.lease.borrow_mut().take();

        Ok(())
    }

    async fn open_status_channel(
        self: Rc<Self>,
        element_name: String,
        status_channel: ServerEnd<fbroker::StatusMarker>,
    ) -> fpt::TopologyControlOpenStatusChannelResult {
        let elements = self.internal_topology.elements.borrow_mut();
        let element = elements.get(&element_name).ok_or_else(|| {
            warn!(element_name:%; "Failed to find element name in the created topology graph");
            fpt::OpenStatusChannelError::InvalidElement
        })?;

        let _ = element.element_control.open_status_channel(status_channel).map_err(|err| {
            warn!(err:%, element_name:%; "Failed to open_status_channel");
            fpt::OpenStatusChannelError::Internal
        })?;

        Ok(())
    }
}
