// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::PowerManagerError;
use crate::message::{Message, MessageReturn};
use crate::node::Node;
use crate::system_shutdown_handler;
use anyhow::{format_err, Context as _, Error};
use async_trait::async_trait;
use fidl::endpoints::Proxy;
use fidl_fuchsia_device_manager as fdevicemgr;
use fidl_fuchsia_hardware_power_statecontrol as fpowerstatecontrol;
use fuchsia_async::{self as fasync};
use fuchsia_inspect::{self as inspect, Property};
use fuchsia_inspect_contrib::{inspect_log, nodes::BoundedListNode};
use fuchsia_zircon as zx;
use log::*;
use std::cell::RefCell;

/// Node: DriverManagerHandler
///
/// Summary: The primary purpose of this node is to set the termination state on the Driver Manager.
///
/// Handles Messages:
///     - SetTerminationSystemState
///
/// Sends Messages: N/A
///
/// FIDL dependencies:
///     - fuchsia.device.manager.SystemStateTransition: a client end of this protocol is provided to
///       at construction. It is used to set the Driver Manager's termination system state.

/// Default handler function that will be called if the fuchsia.device.manager.SystemStateTransition
/// protocol instance that was provided during registration is ever closed.
fn default_termination_channel_closed_handler(result: Result<zx::Signals, zx::Status>) {
    error!("SystemStateTransition channel closed: {:?}. Forcing system shutdown", result);
    fuchsia_trace::instant!(
        "power_manager",
        "DriverManagerHandler::termination_channel_closed_handler",
        fuchsia_trace::Scope::Thread
    );
    system_shutdown_handler::force_shutdown();
}

pub struct DriverManagerHandler {
    termination_state_proxy: fdevicemgr::SystemStateTransitionProxy,

    /// Struct for managing Component Inspection data
    inspect: InspectData,

    /// Task that monitors for closure of `termination_state_proxy` to call
    /// `termination_channel_closed_handler`.
    _monitor_termination_channel_closed_task: fasync::Task<()>,
}

impl DriverManagerHandler {
    pub fn new() -> Result<Self, Error> {
        let termination_state_proxy = fuchsia_component::client::connect_to_protocol::<
            fdevicemgr::SystemStateTransitionMarker,
        >()?;
        Ok(Self::new_with_termination_state_proxy(termination_state_proxy, None))
    }

    fn new_with_termination_state_proxy(
        termination_state_proxy: fdevicemgr::SystemStateTransitionProxy,
        inspect_root: Option<&inspect::Node>,
    ) -> Self {
        Self::new_with_handler(
            termination_state_proxy,
            inspect_root,
            default_termination_channel_closed_handler,
        )
    }

    fn new_with_handler<F: FnOnce(Result<zx::Signals, zx::Status>) + 'static>(
        termination_state_proxy: fdevicemgr::SystemStateTransitionProxy,
        inspect_root: Option<&inspect::Node>,
        termination_channel_closed_handler: F,
    ) -> Self {
        // Optionally use the default inspect root node
        let inspect_root = inspect_root.unwrap_or(inspect::component::inspector().root());
        let inspect = InspectData::new(inspect_root, "DriverManagerHandler".to_string());

        // Create the channel closed handler task
        let monitor_termination_channel_closed_task = {
            let termination_state_proxy = termination_state_proxy.clone();
            fasync::Task::local(async move {
                let result = termination_state_proxy.on_closed().await;
                termination_channel_closed_handler(result)
            })
        };

        Self {
            termination_state_proxy,
            inspect,
            _monitor_termination_channel_closed_task: monitor_termination_channel_closed_task,
        }
    }

    /// Handle the SetTerminationState message. The function uses `termination_state_proxy` to set
    /// the Driver Manager's termination state.
    async fn handle_set_termination_state_message(
        &self,
        state: fpowerstatecontrol::SystemPowerState,
    ) -> Result<MessageReturn, PowerManagerError> {
        // TODO(fxbug.dev/44484): This string must live for the duration of the function because the
        // trace macro uses it when the function goes out of scope. Therefore, it must be bound here
        // and not used anonymously at the macro callsite.
        let state_str = format!("{:?}", state);
        fuchsia_trace::duration!(
            "power_manager",
            "DriverManagerHandler::handle_set_termination_state_message",
            "state" => state_str.as_str()
        );

        let result = self
            .termination_state_proxy
            .set_termination_system_state(state)
            .await
            .context("FIDL failed")?;

        let result = match result.map_err(|e| zx::Status::from_raw(e)) {
            Err(zx::Status::INVALID_ARGS) => Err(PowerManagerError::InvalidArgument(format!(
                "Invalid state argument: {:?}",
                state
            ))),
            Err(e) => Err(PowerManagerError::GenericError(format_err!(
                "SetTerminationSystemState failed: {}",
                e
            ))),
            Ok(()) => Ok(MessageReturn::SetTerminationSystemState),
        };

        fuchsia_trace::instant!(
            "power_manager",
            "DriverManagerHandler::handle_set_termination_state_message_result",
            fuchsia_trace::Scope::Thread,
            "result" => format!("{:?}", result).as_str()
        );

        if let Err(e) = &result {
            self.inspect.log_set_termination_error(format!("{:?}", state), format!("{:?}", e));
        } else {
            self.inspect.termination_state.set(format!("{:?}", state).as_str());
        }

        result
    }
}

#[async_trait(?Send)]
impl Node for DriverManagerHandler {
    fn name(&self) -> String {
        "DriverManagerHandler".to_string()
    }

    async fn handle_message(&self, msg: &Message) -> Result<MessageReturn, PowerManagerError> {
        match msg {
            Message::SetTerminationSystemState(state) => {
                self.handle_set_termination_state_message(*state).await
            }
            _ => Err(PowerManagerError::Unsupported),
        }
    }
}

struct InspectData {
    // Nodes
    _root_node: inspect::Node,

    // Properties
    termination_state: inspect::StringProperty,
    set_termination_errors: RefCell<BoundedListNode>,
}

impl InspectData {
    const NUM_SET_TERMINATION_ERRORS: usize = 10;

    fn new(parent: &inspect::Node, name: String) -> Self {
        let root_node = parent.create_child(name);

        Self {
            termination_state: root_node.create_string("termination_state", "None"),
            set_termination_errors: RefCell::new(BoundedListNode::new(
                root_node.create_child("set_termination_errors"),
                Self::NUM_SET_TERMINATION_ERRORS,
            )),
            _root_node: root_node,
        }
    }

    fn log_set_termination_error(&self, state: String, error: String) {
        inspect_log!(self.set_termination_errors.borrow_mut(), state: state, error: error)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use inspect::assert_data_tree;
    use std::cell::Cell;

    /// Creates a fake implementation of the SystemStateTransition protocol. Responds to the
    /// SetTerminationSystemState request by calling the provided closure. The closure's return
    /// value is returned to the client.
    fn setup_fake_termination_state_service<T>(
        mut set_termination_state: T,
    ) -> fdevicemgr::SystemStateTransitionProxy
    where
        T: FnMut(fpowerstatecontrol::SystemPowerState) -> Result<(), zx::Status> + 'static,
    {
        use futures::TryStreamExt as _;

        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<fdevicemgr::SystemStateTransitionMarker>()
                .unwrap();
        fasync::Task::local(async move {
            while let Ok(req) = stream.try_next().await {
                match req {
                    Some(fdevicemgr::SystemStateTransitionRequest::SetTerminationSystemState {
                        state,
                        responder,
                    }) => {
                        let _ = responder
                            .send(&mut set_termination_state(state).map_err(|e| e.into_raw()));
                    }
                    e => panic!("Unexpected request: {:?}", e),
                }
            }
        })
        .detach();

        proxy
    }

    /// Tests for the presence and correctness of Inspect data
    #[fasync::run_singlethreaded(test)]
    async fn test_inspect_data() {
        let inspector = inspect::Inspector::new();

        // For this test, let the server succeed for the Reboot state but give an
        // error for any other state (so that we can test error paths)
        let termination_state_proxy = setup_fake_termination_state_service(|state| match state {
            fpowerstatecontrol::SystemPowerState::Reboot => Ok(()),
            _ => Err(zx::Status::INVALID_ARGS),
        });

        let node = DriverManagerHandler::new_with_termination_state_proxy(
            termination_state_proxy,
            Some(inspector.root()),
        );

        // Should succeed so `termination_state` will be populated
        let _ = node
            .handle_set_termination_state_message(fpowerstatecontrol::SystemPowerState::Reboot)
            .await;

        // Should fail so `set_termination_errors` will be populated
        let _ = node
            .handle_set_termination_state_message(fpowerstatecontrol::SystemPowerState::FullyOn)
            .await;

        assert_data_tree!(
            inspector,
            root: {
                DriverManagerHandler: {
                    "termination_state": "Reboot",
                    "set_termination_errors": {
                        "0": {
                            "state": "FullyOn",
                            "error": "InvalidArgument(\"Invalid state argument: FullyOn\")",
                            "@time": inspect::testing::AnyProperty
                        }
                    }
                }
            }
        );
    }

    /// Tests that the proxy closure monitor fires after the underlying channel is closed
    #[fasync::run_singlethreaded(test)]
    async fn test_termination_channel_closure() {
        use futures::StreamExt as _;

        // Channel to notify the test when the proxy closure handler has fired
        let (mut channel_closed_sender, mut channel_closed_receiver) =
            futures::channel::mpsc::channel(1);

        // Create the registration parameters
        let (termination_state_proxy, termination_state_server) =
            fidl::endpoints::create_proxy::<fdevicemgr::SystemStateTransitionMarker>().unwrap();

        let _node =
            DriverManagerHandler::new_with_handler(termination_state_proxy, None, move |result| {
                channel_closed_sender.try_send(result).unwrap()
            });

        // Drop the server end to close the channel
        drop(termination_state_server);

        // An item received on `channel_closed_receiver` indicates the proxy closure handler has
        // fired
        assert_matches!(channel_closed_receiver.next().await, Some(Ok(_)));
    }

    /// Tests that the DriverManagerHandler correctly processes the SetTerminationState message by
    /// calling out to the Driver Manager using the termination state proxy.
    #[fasync::run_singlethreaded(test)]
    async fn test_set_termination_state() {
        let termination_state =
            std::rc::Rc::new(Cell::new(fpowerstatecontrol::SystemPowerState::FullyOn));
        let termination_state_clone = termination_state.clone();

        let node = DriverManagerHandler::new_with_termination_state_proxy(
            setup_fake_termination_state_service(move |state| {
                termination_state_clone.set(state);
                Ok(())
            }),
            None,
        );

        // Send the message and verify it returns successfully
        assert_matches!(
            node.handle_message(&Message::SetTerminationSystemState(
                fpowerstatecontrol::SystemPowerState::Reboot
            ))
            .await,
            Ok(MessageReturn::SetTerminationSystemState)
        );

        // Verify the fake termination state service received the correct request
        assert_eq!(termination_state.get(), fpowerstatecontrol::SystemPowerState::Reboot);
    }

    /// Tests for correct ordering of nodes within each available node config file. The
    /// test verifies that if the DriverManagerHandler node is present in the config file, then it
    /// is listed before any other nodes that require a driver connection (identified as a node that
    /// contains a string config key called "driver_path").
    #[test]
    pub fn test_config_files() -> Result<(), anyhow::Error> {
        crate::utils::test_each_node_config_file(|config_file| {
            let driver_manager_handler_index =
                config_file.iter().position(|config| config["type"] == "DriverManagerHandler");
            let first_node_using_drivers_index =
                config_file.iter().position(|config| config["config"].get("driver_path").is_some());

            if driver_manager_handler_index.is_some()
                && first_node_using_drivers_index.is_some()
                && driver_manager_handler_index.unwrap() > first_node_using_drivers_index.unwrap()
            {
                return Err(format_err!(
                    "Must list DriverManagerHandler node before {}",
                    config_file[first_node_using_drivers_index.unwrap()]["name"]
                ));
            }

            Ok(())
        })
    }
}
