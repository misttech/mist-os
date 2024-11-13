// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::CpuManagerError;
use crate::message::{Message, MessageResult, MessageReturn};
use crate::node::Node;
use anyhow::{format_err, Error};
use async_trait::async_trait;
use energy_model_config::PowerLevelDomain;
use fidl_fuchsia_kernel as fkernel;
use fuchsia_inspect::{self as inspect, NumericProperty as _, Property as _};
use std::rc::Rc;
use zx::prelude::AsHandleRef;
use zx::{self as zx, sys};

/// Node: SyscallHandler
///
/// Summary: Executes syscalls so that these calls can be easily mocked by tests for dependent
///          nodes.
///
/// Handles Messages:
///     - GetNumCpus
///     - SetCpuPerformanceInfo
///
/// FIDL dependencies: None

#[derive(Default)]
pub struct SyscallHandlerBuilder<'a> {
    /// A fake CPU resource for injection into SyscallHandler.
    cpu_resource: Option<zx::Resource>,

    /// A fake Inspect root for injection into Syscall Handler.
    inspect_root: Option<&'a inspect::Node>,
}

impl<'a> SyscallHandlerBuilder<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn with_cpu_resource(mut self, resource: zx::Resource) -> Self {
        self.cpu_resource = Some(resource);
        self
    }

    #[cfg(test)]
    fn with_inspect_root(mut self, root: &'a inspect::Node) -> Self {
        self.inspect_root = Some(root);
        self
    }

    pub async fn build(self) -> Result<Rc<SyscallHandler>, Error> {
        // If a CPU resource was not provided, query component_manager for it.
        let cpu_resource = match self.cpu_resource {
            Some(resource) => resource,
            None => {
                let proxy =
                    fuchsia_component::client::connect_to_protocol::<fkernel::CpuResourceMarker>()?;
                proxy.get().await?
            }
        };

        // Optionally use the default inspect root node
        let inspect_root =
            self.inspect_root.unwrap_or_else(|| inspect::component::inspector().root());

        let inspect_data = InspectData::new(inspect_root, "SyscallHandler");

        Ok(Rc::new(SyscallHandler { cpu_resource, inspect: inspect_data }))
    }
}

/// Struct for handling syscalls.
pub struct SyscallHandler {
    cpu_resource: zx::Resource,
    inspect: InspectData,
}

impl SyscallHandler {
    fn handle_get_num_cpus(&self) -> MessageResult {
        // There are no assumptions made by this unsafe block; it is only unsafe due to FFI.
        let num_cpus = unsafe { sys::zx_system_get_num_cpus() };

        Ok(MessageReturn::GetNumCpus(num_cpus))
    }

    fn handle_set_cpu_performance_info(
        &self,
        new_info: &Vec<sys::zx_cpu_performance_info_t>,
    ) -> MessageResult {
        // The unsafe block assumes only that the pointer to `new_info` agrees with the count
        // argument that follows.
        let status = unsafe {
            sys::zx_system_set_performance_info(
                self.cpu_resource.raw_handle(),
                sys::ZX_CPU_PERF_SCALE,
                new_info.as_ptr() as *const u8,
                new_info.len(),
            )
        };

        if let Err(e) = zx::Status::ok(status) {
            self.inspect.set_performance_info_error_count.add(1);
            let error_string = e.to_string();
            self.inspect.set_performance_info_last_error.set(&error_string);
            let verbose_string = format!(
                "{}: zx_system_set_performance_info returned error: {}",
                self.name(),
                &error_string
            );
            tracing::error!("{}", &verbose_string);
            return Err(format_err!("{}", &verbose_string).into());
        }

        Ok(MessageReturn::SetCpuPerformanceInfo)
    }

    fn handle_set_processor_power_domain(
        &self,
        power_level_domain: &PowerLevelDomain,
        port_handle: sys::zx_handle_t,
    ) -> MessageResult {
        let PowerLevelDomain { power_domain, power_levels, power_level_transitions } =
            power_level_domain.clone();
        let status = unsafe {
            sys::zx_system_set_processor_power_domain(
                self.cpu_resource.raw_handle(),
                0,
                &power_domain,
                port_handle,
                power_levels.as_ptr() as *const sys::zx_processor_power_level_t,
                power_levels.len(),
                power_level_transitions.as_ptr()
                    as *const sys::zx_processor_power_level_transition_t,
                power_level_transitions.len(),
            )
        };

        if let Err(e) = zx::Status::ok(status) {
            self.inspect.set_processor_power_domain_error_count.add(1);
            let error_string = e.to_string();
            self.inspect.set_processor_power_domain_last_error.set(&error_string);
            let verbose_string = format!(
                "{}: zx_system_set_processor_power_domain returned error: {}",
                self.name(),
                &error_string
            );
            tracing::error!("{}", &verbose_string);
            return Err(format_err!("{}", &verbose_string).into());
        }

        Ok(MessageReturn::SetCpuProcessorPowerDomain)
    }
}

#[async_trait(?Send)]
impl Node for SyscallHandler {
    fn name(&self) -> String {
        "SyscallHandler".to_string()
    }

    async fn handle_message(&self, msg: &Message) -> MessageResult {
        match msg {
            Message::GetNumCpus => self.handle_get_num_cpus(),
            Message::SetCpuPerformanceInfo(new_info) => {
                self.handle_set_cpu_performance_info(new_info)
            }
            Message::SetCpuProcessorPowerDomain(power_level_domain, port_handle) => {
                self.handle_set_processor_power_domain(power_level_domain, *port_handle)
            }
            _ => Err(CpuManagerError::Unsupported),
        }
    }
}

struct InspectData {
    _root_node: inspect::Node,

    // Properties
    set_performance_info_error_count: inspect::UintProperty,
    set_performance_info_last_error: inspect::StringProperty,

    set_processor_power_domain_error_count: inspect::UintProperty,
    set_processor_power_domain_last_error: inspect::StringProperty,
}

impl InspectData {
    fn new(parent: &inspect::Node, node_name: &str) -> Self {
        let root_node = parent.create_child(node_name);

        let set_performance_info = root_node.create_child("zx_system_set_performance_info");
        let set_performance_info_error_count = set_performance_info.create_uint("error_count", 0);
        let set_performance_info_last_error =
            set_performance_info.create_string("last_error", "<None>");
        root_node.record(set_performance_info);

        let set_processor_power_domain = root_node.create_child("set_processor_power_domain");
        let set_processor_power_domain_error_count =
            set_processor_power_domain.create_uint("error_count", 0);
        let set_processor_power_domain_last_error =
            set_processor_power_domain.create_string("last_error", "<None>");
        root_node.record(set_processor_power_domain);

        Self {
            _root_node: root_node,
            set_performance_info_error_count,
            set_performance_info_last_error,
            set_processor_power_domain_error_count,
            set_processor_power_domain_last_error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::assert_data_tree;
    use fuchsia_async as fasync;
    use zx::sys::{zx_cpu_set_t, zx_processor_power_domain_t};

    // Tests that errors are logged to Inspect as expected.
    #[fasync::run_singlethreaded(test)]
    async fn test_inspect_data() {
        let resource = zx::Handle::invalid().into();
        let inspector = inspect::Inspector::default();
        let builder = SyscallHandlerBuilder::new()
            .with_cpu_resource(resource)
            .with_inspect_root(inspector.root());
        let handler = builder.build().await.unwrap();

        // The test links against a fake implementation of zx_system_set_performance_info that is
        // hard-coded to return ZX_ERR_BAD_HANDLE.
        let set_performance_info_status_string = zx::Status::BAD_HANDLE.to_string();

        match handler.handle_message(&Message::SetCpuPerformanceInfo(Vec::new())).await {
            Ok(_) => panic!("Expected to receive an error"),
            Err(CpuManagerError::GenericError(e)) => {
                assert!(e.to_string().contains(&set_performance_info_status_string));
            }
            Err(e) => panic!("Expected GenericError; received {:?}", e),
        }

        assert_data_tree!(
            inspector,
            root: {
                SyscallHandler: {
                    zx_system_set_performance_info: {
                        error_count: 1u64,
                        last_error: set_performance_info_status_string.clone(),
                    },
                    set_processor_power_domain: {
                        error_count: 0u64,
                        last_error: "<None>".to_string(),
                    }
                }
            }
        );

        // The test links against a fake implementation of
        // zx_system_set_processor_power_domain that is hard-coded to return ZX_ERR_INTERNAL.
        let set_power_level_domain_status_string = zx::Status::INTERNAL.to_string();

        match handler
            .handle_message(&&Message::SetCpuProcessorPowerDomain(
                PowerLevelDomain {
                    power_domain: zx_processor_power_domain_t {
                        cpus: zx_cpu_set_t { mask: [0; 8] },
                        domain_id: 0,
                        padding1: Default::default(),
                    },
                    power_levels: Vec::new(),
                    power_level_transitions: Vec::new(),
                },
                0,
            ))
            .await
        {
            Ok(_) => panic!("Expected to receive an error"),
            Err(CpuManagerError::GenericError(e)) => {
                assert!(e.to_string().contains(&set_power_level_domain_status_string));
            }
            Err(e) => panic!("Expected GenericError; received {:?}", e),
        }

        assert_data_tree!(
            inspector,
            root: {
                SyscallHandler: {
                    zx_system_set_performance_info: {
                        error_count: 1u64,
                        last_error: set_performance_info_status_string,
                    },
                    set_processor_power_domain: {
                        error_count: 1u64,
                        last_error: set_power_level_domain_status_string,
                    }
                }
            }
        );
    }
}
