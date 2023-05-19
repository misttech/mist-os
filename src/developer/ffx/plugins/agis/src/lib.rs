// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use errors::ffx_error;
use ffx_agis_args::{AgisCommand, ListenOp, Operation, RegisterOp};
use ffx_config::keys::TARGET_DEFAULT_KEY;
use fho::{daemon_protocol, moniker, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_developer_ffx::{ListenerProxy, TargetQuery};
use fidl_fuchsia_gpu_agis::{ComponentRegistryProxy, ObserverProxy};
use serde::Serialize;

const GLOBAL_ID: u32 = 1;

#[derive(Serialize, Debug)]
// Vtc == Vulkan Traceable Component
struct Vtc {
    global_id: u32,
    process_koid: u64,
    process_name: String,
}

#[derive(PartialEq)]
struct AgisResult {
    // Some operations return json output.  Others don't.
    json: Option<serde_json::Value>,
}

impl std::fmt::Display for AgisResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.json {
            Some(json) => {
                write!(f, "{}", json.to_string())
            }
            None => {
                write!(f, "{{}}")
            }
        }
    }
}

impl Vtc {
    fn from_fidl(fidl_vtc: fidl_fuchsia_gpu_agis::Vtc) -> Result<Vtc, anyhow::Error> {
        Ok(Vtc {
            global_id: fidl_vtc.global_id.ok_or_else(|| {
                anyhow!(ffx_error!("\"agis\" service error. The \"global_id\" is missing."))
            })?,
            process_koid: fidl_vtc.process_koid.ok_or_else(|| {
                anyhow!(ffx_error!("\"agis\" service error. The \"process_koid\" is missing."))
            })?,
            process_name: fidl_vtc.process_name.ok_or_else(|| {
                anyhow!(ffx_error!("\"agis\" service error. The \"process_name\" is missing."))
            })?,
        })
    }
}

#[derive(FfxTool)]
pub struct AgisTool {
    #[with(moniker("/core/agis"))]
    // fuchsia.gpu.agis.ComponentRegistry
    component_registry: ComponentRegistryProxy,
    // fuchsia.gpu.agis.Observer
    #[with(moniker("/core/agis"))]
    observer: ObserverProxy,
    #[with(daemon_protocol())]
    listener: ListenerProxy,
    #[command]
    cmd: AgisCommand,
}

fho::embedded_plugin!(AgisTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for AgisTool {
    type Writer = SimpleWriter;

    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        agis(self.component_registry, self.observer, self.listener, self.cmd)
            .await
            .map_err(Into::into)
    }
}

async fn agis(
    component_registry: ComponentRegistryProxy,
    observer: ObserverProxy,
    listener: ListenerProxy,
    cmd: AgisCommand,
) -> Result<(), anyhow::Error> {
    println!("{}", agis_impl(component_registry, observer, listener, cmd).await?);
    Ok(())
}

async fn component_registry_register(
    component_registry: ComponentRegistryProxy,
    op: RegisterOp,
) -> Result<AgisResult, anyhow::Error> {
    if op.process_name.is_empty() {
        return Err(anyhow!(ffx_error!("The \"register\" command requires a process name")));
    }
    let result = component_registry.register(op.id, op.process_koid, &op.process_name).await?;
    match result {
        Ok(_) => {
            // Create an arbitrary, valid entry to test as a return value.
            let vtc = Vtc {
                global_id: GLOBAL_ID,
                process_koid: op.process_koid,
                process_name: op.process_name,
            };
            let agis_result = AgisResult { json: Some(serde_json::to_value(&vtc)?) };
            return Ok(agis_result);
        }
        Err(e) => {
            return Err(anyhow!(ffx_error!("The \"register\" command failed with error: {:?}", e)))
        }
    }
}

async fn observer_vtcs(observer: ObserverProxy) -> Result<AgisResult, anyhow::Error> {
    let result = observer.vtcs().await?;
    match result {
        Ok(_fidl_vtcs) => {
            let mut vtcs = vec![];
            for fidl_vtc in _fidl_vtcs {
                let vtc = Vtc::from_fidl(fidl_vtc).unwrap();
                vtcs.push(Vtc {
                    global_id: vtc.global_id,
                    process_name: vtc.process_name,
                    process_koid: vtc.process_koid,
                });
            }
            let mut agis_result = AgisResult { json: None };
            if !vtcs.is_empty() {
                agis_result = AgisResult { json: Some(serde_json::to_value(&vtcs)?) };
            }
            return Ok(agis_result);
        }
        Err(e) => {
            return Err(anyhow!(ffx_error!("The \"vtcs\" command failed with error: {:?}", e)))
        }
    }
}

async fn listener_listen(
    listener: ListenerProxy,
    op: ListenOp,
) -> Result<AgisResult, anyhow::Error> {
    let target_name = ffx_config::get(TARGET_DEFAULT_KEY).await?;
    let target_query = TargetQuery { string_matcher: target_name, ..Default::default() };
    listener
        .listen(&target_query, op.global_id)
        .await?
        .map_err(|e| anyhow!("The \"listen\" command failed with error: {:?}", e))?;

    // No json is needed / returned in AgisResult for this op.
    return Ok(AgisResult { json: None });
}

async fn listener_shutdown(listener: ListenerProxy) -> Result<AgisResult, anyhow::Error> {
    listener
        .shutdown()
        .await?
        .map_err(|e| anyhow!("The \"shutdown\" command failed with error: {:?}", e))?;

    // No json is needed / returned in AgisResult for this op.
    return Ok(AgisResult { json: None });
}

async fn agis_impl(
    component_registry: ComponentRegistryProxy,
    observer: ObserverProxy,
    listener: ListenerProxy,
    cmd: AgisCommand,
) -> Result<AgisResult, anyhow::Error> {
    match cmd.operation {
        Operation::Register(op) => component_registry_register(component_registry, op).await,
        Operation::Vtcs(_) => observer_vtcs(observer).await,
        Operation::Listen(op) => listener_listen(listener, op).await,
        Operation::Shutdown(_) => listener_shutdown(listener).await,
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_fuchsia_developer_ffx::ListenerRequest;
    use fidl_fuchsia_gpu_agis::{ComponentRegistryRequest, ObserverRequest};

    const PROCESS_KOID: u64 = 999;
    const PROCESS_NAME: &str = "agis-vtcs-test";

    fn fake_component_registry() -> ComponentRegistryProxy {
        let callback = move |req| {
            match req {
                ComponentRegistryRequest::Register { responder, .. } => {
                    responder.send(Ok(())).unwrap();
                }
                ComponentRegistryRequest::GetVulkanSocket { responder, .. } => {
                    // Create an arbitrary, valid socket to test as a return value.
                    let (s, _) = fidl::Socket::create_stream();
                    responder.send(Ok(Some(s))).unwrap();
                }
                ComponentRegistryRequest::Unregister { responder, .. } => {
                    responder.send(Ok(())).unwrap();
                }
            };
        };
        fho::testing::fake_proxy(callback)
    }

    fn fake_observer() -> ObserverProxy {
        let callback = move |req| {
            match req {
                ObserverRequest::Vtcs { responder, .. } => {
                    let mut vtcs = vec![];
                    vtcs.push(fidl_fuchsia_gpu_agis::Vtc {
                        global_id: Some(GLOBAL_ID),
                        process_koid: Some(PROCESS_KOID),
                        process_name: Some(PROCESS_NAME.to_string()),
                        ..Default::default()
                    });
                    responder.send(Ok(vtcs)).unwrap();
                }
            };
        };
        fho::testing::fake_proxy(callback)
    }

    fn fake_listener() -> ListenerProxy {
        let callback = move |req| {
            match req {
                ListenerRequest::Listen { responder, .. } => {
                    responder.send(Ok(())).unwrap();
                }
                ListenerRequest::Shutdown { responder, .. } => {
                    responder.send(Ok(())).unwrap();
                }
            };
        };
        fho::testing::fake_proxy(callback)
    }

    #[fuchsia_async::run_singlethreaded(test)]
    pub async fn register() {
        let cmd = AgisCommand {
            operation: Operation::Register(RegisterOp {
                id: 0u64,
                process_koid: 0u64,
                process_name: "agis-register".to_string(),
            }),
        };

        let component_registry = fake_component_registry();
        let observer = fake_observer();
        let mut listener = fake_listener();
        let mut result = agis_impl(component_registry, observer, listener, cmd).await;
        result.unwrap();

        let no_name_cmd = AgisCommand {
            operation: Operation::Register(RegisterOp {
                id: 0u64,
                process_koid: 0u64,
                process_name: "".to_string(),
            }),
        };
        let no_name_component_registry = fake_component_registry();
        let observer = fake_observer();
        listener = fake_listener();
        result = agis_impl(no_name_component_registry, observer, listener, no_name_cmd).await;
        assert!(result.is_err());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    pub async fn vtcs() {
        let cmd = AgisCommand { operation: Operation::Vtcs(ffx_agis_args::VtcsOp {}) };
        let component_registry = fake_component_registry();
        let observer = fake_observer();
        let listener = fake_listener();
        let result = agis_impl(component_registry, observer, listener, cmd).await;
        let expected_output = serde_json::json!([{
            "global_id": GLOBAL_ID,
            "process_koid": PROCESS_KOID,
            "process_name": PROCESS_NAME,
        }]);
        assert_eq!(result.unwrap().json.unwrap(), expected_output);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    pub async fn listen() {
        let cmd = AgisCommand {
            operation: Operation::Listen(ffx_agis_args::ListenOp { global_id: GLOBAL_ID }),
        };
        let component_registry = fake_component_registry();
        let observer = fake_observer();
        let listener = fake_listener();
        let result = agis_impl(component_registry, observer, listener, cmd).await;
        assert!(result.is_err());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    pub async fn shutdown() {
        let cmd = AgisCommand { operation: Operation::Shutdown(ffx_agis_args::ShutdownOp {}) };
        let component_registry = fake_component_registry();
        let observer = fake_observer();
        let listener = fake_listener();
        let result = agis_impl(component_registry, observer, listener, cmd).await;
        assert!(!result.is_err());
    }
}
