// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common_utils::result_debug_panic::ResultDebugPanic;
use crate::message::{Message, MessageReturn};
use crate::node::Node;
use anyhow::{Error, Result};
use async_trait::async_trait;
use energy_model_config::{EnergyModel, PowerLevelDomain};
use fidl::AsHandleRef;
use fuchsia_inspect::ArrayProperty;
use futures::future::{FutureExt as _, LocalBoxFuture};
use futures::stream::FuturesUnordered;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use zx::sys;
use {fuchsia_inspect as inspect, serde_json as json};

/// Node: RppmHandler
///
/// Summary: Loads the energy model configuration json file from the device,
/// registers the energy model with kernel and services the OPP change requests
/// from kernel for runtime processor power management.
///
/// Sends Messages:
///   - SetProcessorPowerDomain
///   - SetProcessorPowerState
///   - SetOperatingPoint
///   - GetOperatingPoint
///
/// FIDL dependencies: No direct dependencies

pub struct RppmHandlerBuilder<'a> {
    // Name of the node, specified in config file.
    node_name: String,
    // Allow test to inject a fake energy model.
    energy_model: Option<EnergyModel>,
    // Allow test to inject a fake inspect root.
    inspect_root: Option<&'a inspect::Node>,
    // This node handles the SetProcessorPowerDomain and SetProcessorPowerState
    // messages.
    syscall_handler: Rc<dyn Node>,
    // Map from domain id to the node that handles the SetOperatingPoint
    // and GetOperatingPoint message for this domain.
    power_domain_handlers: HashMap<u32, Rc<dyn Node>>,
}

impl<'a> RppmHandlerBuilder<'a> {
    /// Default path to the energy model config file.
    const ENERGY_MODEL_CONFIG_PATH: &'static str = "/config/energy_model_config.json";

    #[cfg(test)]
    pub fn new() -> Self {
        use crate::test::mock_node::create_dummy_node;
        Self {
            node_name: "RppmHandler".to_string(),
            energy_model: None,
            syscall_handler: create_dummy_node(),
            power_domain_handlers: HashMap::new(),
            inspect_root: None,
        }
    }

    #[cfg(test)]
    fn with_energy_model(mut self, energy_model: EnergyModel) -> Self {
        self.energy_model = Some(energy_model);
        self
    }

    #[cfg(test)]
    fn with_inspect_root(mut self, root: &'a inspect::Node) -> Self {
        self.inspect_root = Some(root);
        self
    }

    pub fn new_from_json(json_data: json::Value, nodes: &HashMap<String, Rc<dyn Node>>) -> Self {
        #[derive(Deserialize)]
        struct Config {
            power_domain_handlers: Vec<PowerDomainHandler>,
        }

        #[derive(Deserialize)]
        struct PowerDomainHandler {
            domain_id: u32,
            handler: String,
        }

        #[derive(Deserialize)]
        struct Dependencies {
            cpu_device_handlers: Vec<String>,
            syscall_handler: String,
        }

        #[derive(Deserialize)]
        struct JsonData {
            name: String,
            config: Config,
            dependencies: Dependencies,
        }

        let data: JsonData = json::from_value(json_data).unwrap();

        assert_eq!(
            data.config.power_domain_handlers.iter().map(|c| &c.handler).collect::<Vec<_>>(),
            data.dependencies.cpu_device_handlers.iter().collect::<Vec<_>>()
        );

        Self {
            node_name: data.name,
            energy_model: None,
            power_domain_handlers: data
                .config
                .power_domain_handlers
                .iter()
                .map(|p| (p.domain_id, nodes[&p.handler].clone()))
                .collect(),
            syscall_handler: nodes[&data.dependencies.syscall_handler].clone(),
            inspect_root: None,
        }
    }

    pub async fn build(
        self,
        futures_out: &FuturesUnordered<LocalBoxFuture<'_, ()>>,
    ) -> Result<Rc<RppmHandler>, Error> {
        let energy_model = match self.energy_model {
            Some(energy_model) => energy_model,
            None => {
                // Read the energy model config file.
                EnergyModel::new(&Path::new(&Self::ENERGY_MODEL_CONFIG_PATH.to_string()))
                    .or_debug_panic()?
            }
        };

        // Optionally use the default inspect root node
        let inspect_root =
            self.inspect_root.unwrap_or_else(|| inspect::component::inspector().root());
        let inspect_data = InspectData::new(inspect_root, &self.node_name);
        inspect_data.log_energy_model(&energy_model);

        let node = Rc::new(RppmHandler {
            energy_model,
            syscall_handler: self.syscall_handler,
            power_domain_handlers: self.power_domain_handlers,
            _inspect: inspect_data,
        });

        futures_out.push(node.clone().run());
        Ok(node)
    }
}

/// The RppmHandler node.
pub struct RppmHandler {
    energy_model: EnergyModel,
    syscall_handler: Rc<dyn Node>,
    power_domain_handlers: HashMap<u32, Rc<dyn Node>>,
    _inspect: InspectData,
}

impl RppmHandler {
    fn run<'a>(self: Rc<Self>) -> LocalBoxFuture<'a, ()> {
        async move {
            // TODO(https://fxbug.dev/375533194): Add unit tests for handling port messages.
            let port = zx::Port::create();

            for power_level_domain in self.energy_model.0.clone() {
                // Register energy model with kernel.
                self.register_energy_model(port.raw_handle(), power_level_domain.clone()).await;
                let domain_id = power_level_domain.power_domain.domain_id;

                // Query initial pstate from CPU driver and send it to kernel.
                // `sys::zx_processor_power_state_t::control_argument` should match the opp
                // used in fuchsia.hardware.cpu.ctrl.
                match self
                    .power_domain_handlers
                    .get(&domain_id)
                    .unwrap()
                    .handle_message(&Message::GetOperatingPoint)
                    .await
                {
                    Ok(MessageReturn::GetOperatingPoint(opp)) => {
                        let pstate = sys::zx_processor_power_state_t {
                            domain_id,
                            control_interface: sys::ZX_PROCESSOR_POWER_CONTROL_CPU_DRIVER,
                            control_argument: opp as u64,
                            options: 0,
                        };
                        self.set_processor_power_state(port.raw_handle(), pstate).await;
                    }
                    result => {
                        tracing::error!("Unexpected result from GetOperatingPoint: {:?}", result);
                    }
                }
            }

            loop {
                match port.wait(zx::MonotonicInstant::INFINITE) {
                    Err(e) => tracing::error!("zx_port_wait failed with error: {}", e),
                    Ok(p) => {
                        match p.contents() {
                            zx::PacketContents::PowerTransition(packet) => {
                                assert_eq!(
                                    packet.control_interface(),
                                    sys::ZX_PROCESSOR_POWER_CONTROL_CPU_DRIVER
                                );
                                assert_eq!(packet.options(), 0);
                                let control_argument = packet.control_argument();

                                // Request CPU driver to make OPP change.
                                let msg = Message::SetOperatingPoint(
                                    control_argument.try_into().unwrap(),
                                );
                                match self
                                    .power_domain_handlers
                                    .get(&packet.domain_id())
                                    .unwrap()
                                    .handle_message(&msg)
                                    .await
                                {
                                    Ok(MessageReturn::SetOperatingPoint) => {
                                        // Notify the kernel about the updated OPP.
                                        let pstate = sys::zx_processor_power_state_t {
                                            domain_id: packet.domain_id(),
                                            control_interface:
                                                sys::ZX_PROCESSOR_POWER_CONTROL_CPU_DRIVER,
                                            control_argument,
                                            options: 0,
                                        };
                                        self.set_processor_power_state(port.raw_handle(), pstate)
                                            .await;
                                    }
                                    Ok(other) => {
                                        panic!("Unexpected SetOperatingPoint result: {:?}", other)
                                    }
                                    Err(e) => {
                                        tracing::error!("Error requesting OPP change: {}", e);
                                    }
                                };
                            }
                            _ => panic!("wrong packet type"),
                        }
                    }
                }
            }
        }
        .boxed_local()
    }

    async fn register_energy_model(
        &self,
        handle: sys::zx_handle_t,
        power_level_domain: PowerLevelDomain,
    ) {
        let msg = Message::SetProcessorPowerDomain(power_level_domain, handle);
        match self.syscall_handler.handle_message(&msg).await {
            Ok(MessageReturn::SetProcessorPowerDomain) => (),
            Ok(other) => {
                panic!("Unexpected SetProcessorPowerDomain result: {:?}", other)
            }
            Err(e) => {
                tracing::error!("Error sending energy model to kernel: {}", e);
            }
        };
    }

    async fn set_processor_power_state(
        &self,
        handle: sys::zx_handle_t,
        pstate: sys::zx_processor_power_state_t,
    ) {
        let msg = Message::SetProcessorPowerState(handle, pstate);
        match self.syscall_handler.handle_message(&msg).await {
            Ok(MessageReturn::SetProcessorPowerState) => (),
            Ok(other) => {
                panic!("Unexpected SetProcessorPowerState result: {:?}", other)
            }
            Err(e) => {
                tracing::error!("Error sending updated power state to kernel: {}", e);
            }
        };
    }
}

#[async_trait(?Send)]
impl Node for RppmHandler {
    fn name(&self) -> String {
        "RppmHandler".to_string()
    }
}

struct InspectData {
    root_node: inspect::Node,
}

impl InspectData {
    fn new(parent: &inspect::Node, node_name: &str) -> Self {
        let root_node = parent.create_child(node_name);
        Self { root_node }
    }

    fn log_energy_model(&self, energy_model: &EnergyModel) {
        let energy_model_node = self.root_node.create_child("energy model");

        for power_level_domain in energy_model.0.clone() {
            let domain_node = energy_model_node
                .create_child(power_level_domain.power_domain.domain_id.to_string());

            let cpu_set_mask_node = domain_node
                .create_uint_array("cpu_set_mask", power_level_domain.power_domain.cpus.mask.len());
            power_level_domain
                .power_domain
                .cpus
                .mask
                .iter()
                .enumerate()
                .for_each(|(i, p)| cpu_set_mask_node.set(i, *p as u64));
            domain_node.record(cpu_set_mask_node);

            let power_levels_node = domain_node.create_child("power_levels");
            for (index, power_level) in
                power_level_domain.power_levels.clone().into_iter().enumerate()
            {
                let power_level_node = power_levels_node.create_child(index.to_string());
                power_level_node.record_uint("option", power_level.options);
                power_level_node
                    .record_uint("power_coefficient_nw", power_level.power_coefficient_nw);
                power_level_node.record_uint("processing_rate", power_level.processing_rate);
                power_level_node.record_uint("control_interface", power_level.control_interface);
                power_level_node.record_uint("control_argument", power_level.control_argument);

                let diagnostic_name_node = power_level_node
                    .create_uint_array("diagnostic_name", power_level.diagnostic_name.len());
                power_level
                    .diagnostic_name
                    .iter()
                    .enumerate()
                    .for_each(|(i, p)| diagnostic_name_node.set(i, *p as u64));
                power_level_node.record(diagnostic_name_node);
                power_levels_node.record(power_level_node);
            }
            domain_node.record(power_levels_node);

            let power_level_transitions_node = domain_node.create_child("power_level_transitions");
            for (index, power_level_transition) in
                power_level_domain.power_level_transitions.clone().into_iter().enumerate()
            {
                let power_level_transition_node =
                    power_level_transitions_node.create_child(index.to_string());
                power_level_transition_node.record_uint("from", power_level_transition.from.into());
                power_level_transition_node.record_uint("to", power_level_transition.to.into());
                power_level_transition_node.record_int("latency", power_level_transition.latency);
                power_level_transition_node.record_uint("energy", power_level_transition.energy);
                power_level_transitions_node.record(power_level_transition_node);
            }
            domain_node.record(power_level_transitions_node);

            energy_model_node.record(domain_node);
        }

        self.root_node.record(energy_model_node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::mock_node::create_dummy_node;
    use diagnostics_assertions::assert_data_tree;
    use fuchsia_async as fasync;
    use zx::sys::{
        zx_cpu_set_t, zx_processor_power_domain_t, zx_processor_power_level_t,
        zx_processor_power_level_transition_t,
    };

    /// Tests that well-formed configuration JSON does not panic the `new_from_json` function.
    #[test]
    fn test_new_from_json() {
        let json_data = json::json!({
            "type": "RppmHandler",
            "name": "energy_model_handler",
            "config": {
                "power_domain_handlers": [
                    {
                        "domain_id": 0,
                        "handler": "cpu_device_handler",
                    },
                ],
            },
            "dependencies": {
                "syscall_handler": "syscall_handler",
                "cpu_device_handlers": ["cpu_device_handler"],
            }
        });

        let mut nodes: HashMap<String, Rc<dyn Node>> = HashMap::new();
        nodes.insert("syscall_handler".to_string(), create_dummy_node());
        nodes.insert("cpu_device_handler".to_string(), create_dummy_node());
        let _ = RppmHandlerBuilder::new_from_json(json_data, &nodes);
    }

    /// Tests for the presence and correctness of inspect data
    #[test]
    fn test_inspect_data() {
        let mut exec = fasync::TestExecutor::new();
        let inspector = inspect::Inspector::default();
        let futures_out = FuturesUnordered::new();
        let energy_model = EnergyModel(vec![PowerLevelDomain {
            power_domain: zx_processor_power_domain_t {
                cpus: zx_cpu_set_t { mask: [0; 8] },
                domain_id: 0,
                padding1: Default::default(),
            },
            power_levels: vec![
                zx_processor_power_level_t {
                    options: 0,
                    processing_rate: 100,
                    power_coefficient_nw: 200,
                    control_interface: 2,
                    control_argument: 1,
                    diagnostic_name: [0; 32],
                    padding: [0; 32],
                },
                zx_processor_power_level_t {
                    options: 1,
                    processing_rate: 200,
                    power_coefficient_nw: 300,
                    control_interface: 0,
                    control_argument: 0,
                    diagnostic_name: [0; 32],
                    padding: [0; 32],
                },
            ],
            power_level_transitions: vec![zx_processor_power_level_transition_t {
                from: 0,
                to: 1,
                energy: 200,
                latency: 100,
                padding: [0; 6],
            }],
        }]);

        let _node = exec.run_until_stalled(
            &mut RppmHandlerBuilder::new()
                .with_energy_model(energy_model)
                .with_inspect_root(inspector.root())
                .build(&futures_out)
                .boxed_local(),
        );

        assert_data_tree!(
            inspector,
            root: {
                "RppmHandler": {
                    "energy model": {
                        "0": {
                            cpu_set_mask: vec![0u64; 8],
                            power_levels: {
                                "0": {
                                    option: 0u64,
                                    processing_rate: 100u64,
                                    power_coefficient_nw: 200u64,
                                    control_interface: 2u64,
                                    control_argument: 1u64,
                                    diagnostic_name: vec![0u64; 32],
                                },
                                "1": {
                                    option: 1u64,
                                    processing_rate: 200u64,
                                    power_coefficient_nw: 300u64,
                                    control_interface: 0u64,
                                    control_argument: 0u64,
                                    diagnostic_name: vec![0u64; 32],
                                },
                            },
                            power_level_transitions: {
                                "0": {
                                    from: 0u64,
                                    to: 1u64,
                                    energy: 200u64,
                                    latency: 100i64,
                                },
                            },
                        }
                    }
                }
            }
        );
    }
}
