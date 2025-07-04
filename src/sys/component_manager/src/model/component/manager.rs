// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::{ComponentInstance, RouterError, RouterResponse};
use ::routing::capability_source::{BuiltinCapabilities, NamespaceCapabilities};
use ::routing::component_instance::TopInstanceInterface;
use async_trait::async_trait;
use cm_util::TaskGroup;
use errors::RebootError;
use fidl::endpoints::{self};
use fuchsia_component::client;
use log::warn;
use routing::error::RoutingError;
use sandbox::{Connector, Request, Routable};
use std::sync::{Arc, Mutex};
use vfs::directory::entry::OpenRequest;
use vfs::path::Path;
use vfs::ToObjectRequest;
use {
    fidl_fuchsia_hardware_power_statecontrol as fstatecontrol, fidl_fuchsia_io as fio,
    fuchsia_async as fasync,
};

/// A special instance identified with component manager, at the top of the tree.
#[derive(Debug)]
pub struct ComponentManagerInstance {
    /// The list of capabilities offered from component manager's namespace.
    pub namespace_capabilities: NamespaceCapabilities,

    /// The list of capabilities offered from component manager as built-in capabilities.
    pub builtin_capabilities: BuiltinCapabilities,

    /// Tasks owned by component manager's instance.
    task_group: TaskGroup,

    /// Mutable state for component manager's instance.
    state: Mutex<ComponentManagerInstanceState>,
}

/// Mutable state for component manager's instance.
#[derive(Debug)]
pub struct ComponentManagerInstanceState {
    /// The root component instance, this instance's only child.
    root: Option<Arc<ComponentInstance>>,

    /// Task that is rebooting the system, if any.
    reboot_task: Option<fasync::Task<()>>,
}

impl ComponentManagerInstance {
    pub fn new(
        namespace_capabilities: NamespaceCapabilities,
        builtin_capabilities: BuiltinCapabilities,
    ) -> Self {
        Self {
            namespace_capabilities,
            builtin_capabilities,
            state: Mutex::new(ComponentManagerInstanceState::new()),
            task_group: TaskGroup::new(),
        }
    }

    /// Returns a group where tasks can be run scoped to this instance
    pub fn task_group(&self) -> TaskGroup {
        self.task_group.clone()
    }

    #[cfg(all(test, feature = "src_model_tests"))]
    pub fn has_reboot_task(&self) -> bool {
        self.state.lock().unwrap().reboot_task.is_some()
    }

    /// Returns the root component instance.
    ///
    /// REQUIRES: The root has already been set. Otherwise panics.
    pub fn root(&self) -> Arc<ComponentInstance> {
        self.state.lock().unwrap().root.as_ref().expect("root not set").clone()
    }

    /// Returns a connector that lazily resolves the given protocol exposed by `/`.
    ///
    /// REQUIRES: The root has already been set. Otherwise panics.
    // TODO(https://fxbug.dev/422537652): The allow(dead_code) is currently necessary because
    // this function is only called if #[cfg(feature="heapdump")]. The next CL in the stack
    // will also call it from other places that are not conditionally compiled.
    #[allow(dead_code)]
    pub fn get_root_exposed_capability_router(
        &self,
        source_name: cm_types::Name,
    ) -> impl Routable<Connector> {
        struct RootCapabilityRouter {
            root: Arc<ComponentInstance>,
            source_name: cm_types::Name,
        }

        #[async_trait]
        impl Routable<Connector> for RootCapabilityRouter {
            async fn route(
                &self,
                request: Option<Request>,
                debug: bool,
            ) -> Result<RouterResponse<Connector>, RouterError> {
                let component_output = self
                    .root
                    .lock_resolved_state()
                    .await
                    .map_err(|e| RouterError::NotFound(Arc::new(e)))?
                    .sandbox
                    .component_output
                    .clone();
                match component_output.capabilities().get(&self.source_name) {
                    Ok(Some(sandbox::Capability::ConnectorRouter(router))) => {
                        Ok(router.route(request, debug).await?)
                    }
                    _ => Err(RouterError::NotFound(Arc::new(
                        RoutingError::UseFromRootExposeNotFound {
                            capability_id: self.source_name.to_string(),
                        },
                    ))),
                }
            }
        }

        RootCapabilityRouter { root: self.root(), source_name }
    }

    /// Initializes the state of the instance. Panics if already initialized.
    pub fn init(&self, root: Arc<ComponentInstance>) {
        let mut state = self.state.lock().unwrap();
        assert!(state.root.is_none(), "child of top instance already set");
        state.root = Some(root);
    }

    /// Triggers a graceful system reboot. Panics if the reboot call fails (which will trigger a
    /// forceful reboot if this is the root component manager instance).
    ///
    /// Returns as soon as the call has been made. In the background, component_manager will wait
    /// on the `Reboot` call.
    pub(super) fn trigger_reboot(self: &Arc<Self>) {
        let mut state = self.state.lock().unwrap();
        if state.reboot_task.is_some() {
            // Reboot task was already scheduled, nothing to do.
            return;
        }
        let this = self.clone();
        state.reboot_task = Some(fasync::Task::spawn(async move {
            let res = async move {
                let statecontrol_proxy = this.connect_to_statecontrol_admin().await?;
                statecontrol_proxy
                    .perform_reboot(&fstatecontrol::RebootOptions {
                        reasons: Some(vec![fstatecontrol::RebootReason2::CriticalComponentFailure]),
                        ..Default::default()
                    })
                    .await?
                    .map_err(|s| RebootError::AdminError(zx::Status::from_raw(s)))
            }
            .await;
            if let Err(RebootError::AdminError(zx::Status::ALREADY_EXISTS)) = res {
                warn!("Reboot in progress but trigger_reboot is called again. Something else is going on.");
                return;
            }
            if let Err(e) = res {
                // TODO(https://fxbug.dev/42161535): Instead of panicking, we could fall back more gently by
                // triggering component_manager's shutdown.
                panic!(
                    "Component with on_terminate=REBOOT terminated, but triggering \
                    reboot failed. Crashing component_manager instead: {}",
                    e
                );
            }
        }));
    }

    /// Obtains a connection to power_manager's `statecontrol` protocol.
    async fn connect_to_statecontrol_admin(
        &self,
    ) -> Result<fstatecontrol::AdminProxy, RebootError> {
        let (exposed_dir, server) = endpoints::create_proxy::<fio::DirectoryMarker>();
        let root = self.root();
        const FLAGS: fio::Flags = fio::PERM_READABLE;
        let mut object_request = FLAGS.to_object_request(server);
        root.open_exposed(OpenRequest::new(
            root.execution_scope.clone(),
            FLAGS,
            Path::dot(),
            &mut object_request,
        ))
        .await?;
        let statecontrol_proxy =
            client::connect_to_protocol_at_dir_root::<fstatecontrol::AdminMarker>(&exposed_dir)
                .map_err(RebootError::ConnectToAdminFailed)?;
        Ok(statecontrol_proxy)
    }
}

impl ComponentManagerInstanceState {
    pub fn new() -> Self {
        Self { reboot_task: None, root: None }
    }
}

impl TopInstanceInterface for ComponentManagerInstance {
    fn namespace_capabilities(&self) -> &NamespaceCapabilities {
        &self.namespace_capabilities
    }

    fn builtin_capabilities(&self) -> &BuiltinCapabilities {
        &self.builtin_capabilities
    }
}
