// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::PowerManagerError;
use crate::message::{Message, MessageReturn};
use crate::node::Node;
use anyhow::{format_err, Error};
use async_trait::async_trait;
use fidl_fuchsia_hardware_power_statecontrol as fpowercontrol;
use fuchsia_inspect::{self as inspect, Property};
use log::*;
use std::rc::Rc;
/// Node: SystemShutdownHandler
///
/// Summary: Provides a mechanism for the Power Manager to shut down the system due to either
/// extreme temperature or other reasons.
///
/// Handles Messages:
///     - SystemShutdown
///
/// FIDL dependencies:
///     - fuchsia.hardware.power.statecontrol.Admin: the node uses this protocol in case the
///       temperature exceeds a threshold. If the call fails, Power Manager will force a shutdown
///       by terminating itself.

/// A builder for constructing the SystemShutdownHandler node.
pub struct SystemShutdownHandlerBuilder<'a> {
    shutdown_shim_proxy: Option<fpowercontrol::AdminProxy>,
    force_shutdown_func: Box<dyn Fn()>,
    inspect_root: Option<&'a inspect::Node>,
}

impl<'a> SystemShutdownHandlerBuilder<'a> {
    pub fn new() -> Self {
        Self {
            shutdown_shim_proxy: None,
            force_shutdown_func: Box::new(force_shutdown),
            inspect_root: None,
        }
    }

    #[cfg(test)]
    pub fn with_shutdown_shim_proxy(mut self, proxy: fpowercontrol::AdminProxy) -> Self {
        self.shutdown_shim_proxy = Some(proxy);
        self
    }

    #[cfg(test)]
    pub fn with_force_shutdown_function(
        mut self,
        force_shutdown: Box<impl Fn() + 'static>,
    ) -> Self {
        self.force_shutdown_func = force_shutdown;
        self
    }

    #[cfg(test)]
    pub fn with_inspect_root(mut self, root: &'a inspect::Node) -> Self {
        self.inspect_root = Some(root);
        self
    }

    pub fn build(self) -> Result<Rc<SystemShutdownHandler>, Error> {
        // Optionally use the default inspect root node
        let inspect_root =
            self.inspect_root.unwrap_or_else(|| inspect::component::inspector().root());

        // Connect to the shutdown-shim's Admin service if a proxy wasn't specified
        let shutdown_shim_proxy = if let Some(proxy) = self.shutdown_shim_proxy {
            proxy
        } else {
            fuchsia_component::client::connect_to_protocol::<fpowercontrol::AdminMarker>()?
        };

        let node = Rc::new(SystemShutdownHandler {
            force_shutdown_func: self.force_shutdown_func,
            shutdown_shim_proxy,
            inspect: InspectData::new(inspect_root, "SystemShutdownHandler".to_string()),
        });

        Ok(node)
    }
}

pub struct SystemShutdownHandler {
    /// Function to force a system shutdown.
    force_shutdown_func: Box<dyn Fn()>,

    /// Proxy handle to communicate with the Shutdown-shim's Admin protocol.
    shutdown_shim_proxy: fpowercontrol::AdminProxy,

    /// Struct for managing Component Inspection data
    inspect: InspectData,
}

impl SystemShutdownHandler {
    /// Called only when there is a high temperature reboot request.
    /// If the function is called while a shutdown is already in
    /// progress, then an error is returned. This is the only scenario where the function will
    /// return. In all other cases, the function does not return.
    async fn handle_shutdown(&self, msg: &Message) -> Result<(), Error> {
        fuchsia_trace::instant!(
            c"power_manager",
            c"SystemShutdownHandler::handle_shutdown",
            fuchsia_trace::Scope::Thread,
            "msg" => format!("{:?}", msg).as_str()
        );

        info!("System shutdown ({:?})", msg);
        self.inspect.log_shutdown_request(&msg);
        // TODO: Use perform_reboot(RebootReasons::new(RebootReason2::HighTemperature,)))
        let result =
            self.shutdown_shim_proxy.reboot(fpowercontrol::RebootReason::HighTemperature).await;

        // If the result is an error, either by underlying API failure or by timeout, then force a
        // shutdown using the configured force_shutdown_func
        if result.is_err() {
            self.inspect.force_shutdown_attempted.set(true);
            (self.force_shutdown_func)();
        }

        Ok(())
    }

    /// Handle a SystemShutdown message which is a request to shut down the system.
    async fn handle_system_shutdown_message(
        &self,
        msg: &Message,
    ) -> Result<MessageReturn, PowerManagerError> {
        match self.handle_shutdown(msg).await {
            Ok(()) => Ok(MessageReturn::SystemShutdown),
            Err(e) => Err(PowerManagerError::GenericError(format_err!("{}", e))),
        }
    }
}

/// Forcibly shuts down the system. The function works by exiting the power_manager process. Since
/// the power_manager is marked as a critical process to the root job, once the power_manager exits
/// the root job will also exit, and the system will reboot.
pub fn force_shutdown() {
    info!("Force shutdown requested");
    std::process::exit(1);
}

#[async_trait(?Send)]
impl Node for SystemShutdownHandler {
    fn name(&self) -> String {
        "SystemShutdownHandler".to_string()
    }

    async fn handle_message(&self, msg: &Message) -> Result<MessageReturn, PowerManagerError> {
        match msg {
            Message::HighTemperatureReboot => self.handle_system_shutdown_message(msg).await,
            _ => Err(PowerManagerError::Unsupported),
        }
    }
}

struct InspectData {
    // Nodes
    _root_node: inspect::Node,

    // Properties
    shutdown_request: inspect::StringProperty,
    force_shutdown_attempted: inspect::BoolProperty,
}

impl InspectData {
    fn new(parent: &inspect::Node, name: String) -> Self {
        let root_node = parent.create_child(name);
        Self {
            shutdown_request: root_node.create_string("shutdown_request", "None"),
            force_shutdown_attempted: root_node.create_bool("force_shutdown_attempted", false),
            _root_node: root_node,
        }
    }

    /// Updates the `shutdown_request` property according to the provided request.
    fn log_shutdown_request(&self, request: &Message) {
        self.shutdown_request.set(format!("{:?}", request).as_str());
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use diagnostics_assertions::assert_data_tree;
    use fuchsia_async as fasync;
    use futures::TryStreamExt;
    use std::cell::Cell;

    /// Create a fake Admin service proxy that responds to Shutdown requests by calling
    /// the provided closure.
    fn setup_fake_admin_service(
        mut shutdown_function: impl FnMut() + 'static,
    ) -> fpowercontrol::AdminProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<fpowercontrol::AdminMarker>();
        fasync::Task::local(async move {
            while let Ok(req) = stream.try_next().await {
                match req {
                    Some(fpowercontrol::AdminRequest::Reboot { reason: _, responder }) => {
                        shutdown_function();
                        let _ = responder.send(Ok(()));
                    }
                    e => panic!("Unexpected request: {:?}", e),
                }
            }
        })
        .detach();

        proxy
    }

    /// Tests for the presence and correctness of inspect data
    #[fasync::run_singlethreaded(test)]
    async fn test_inspect_data() {
        let inspector = inspect::Inspector::default();
        let node = SystemShutdownHandlerBuilder::new()
            .with_inspect_root(inspector.root())
            .with_force_shutdown_function(Box::new(|| {}))
            .build()
            .unwrap();

        // Issue a shutdown call that will fail (because the Component Manager mock node will
        // respond to Shutdown with an error), which causes a force shutdown to be issued.
        // This gives us something interesting to verify in Inspect.
        let _ = node.handle_shutdown(&Message::HighTemperatureReboot).await;

        assert_data_tree!(
            inspector,
            root: {
                SystemShutdownHandler: {
                    shutdown_request: "HighTemperatureReboot",
                    force_shutdown_attempted: true
                }
            }
        );
    }

    /// Tests that the `shutdown` function correctly sets the corresponding termination state on the
    /// Driver Manager and calls the Component Manager shutdown API.
    #[fasync::run_singlethreaded(test)]
    async fn test_shutdown() {
        // At the end of the test, verify the Component Manager's received shutdown count (expected
        // to be equal to the number of entries in the system_power_states vector)
        let shutdown_count = Rc::new(Cell::new(0));
        let shutdown_count_clone = shutdown_count.clone();

        // Create the node with a special Component Manager proxy
        let node = SystemShutdownHandlerBuilder::new()
            .with_shutdown_shim_proxy(setup_fake_admin_service(move || {
                shutdown_count_clone.set(shutdown_count_clone.get() + 1);
            }))
            .build()
            .unwrap();

        // Call `suspend` for each power state.
        let _ = node.handle_shutdown(&Message::HighTemperatureReboot).await;

        // Verify the Component Manager shutdown was called
        assert_eq!(shutdown_count.get(), 1);
    }

    /// Tests that if an orderly shutdown request fails, the forced shutdown method is called.
    #[fasync::run_singlethreaded(test)]
    async fn test_force_shutdown() {
        let force_shutdown = Rc::new(Cell::new(false));
        let force_shutdown_clone = force_shutdown.clone();
        let force_shutdown_func = Box::new(move || {
            force_shutdown_clone.set(true);
        });

        let node = SystemShutdownHandlerBuilder::new()
            .with_force_shutdown_function(force_shutdown_func)
            .build()
            .unwrap();

        // Call the normal shutdown function. The orderly shutdown will fail because the
        // Component manager mock node will respond to the Shutdown message with an
        // error. When the orderly shutdown fails, the forced shutdown method will be called.
        let _ = node.handle_shutdown(&Message::HighTemperatureReboot).await;
        assert_eq!(force_shutdown.get(), true);
    }
}
