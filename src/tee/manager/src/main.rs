// Copyright 2024 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Error;
use fidl_fuchsia_tee::ApplicationRequestStream;
use fuchsia_async as fasync;
use fuchsia_component::{
    client::{connect_to_protocol, connect_to_protocol_at_dir_root},
    server::ServiceFs,
};
use futures::prelude::*;
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
struct TAConfig {
    url: String,
    single_instance: bool,
    capabilities: Vec<()>,
}

struct TAConnectRequest {
    uuid: String,
    stream: ApplicationRequestStream,
}

async fn run_application(mut request: TAConnectRequest, config: TAConfig) {
    // TODO: Check the config to see if this is singleInstance. If so, look for an existing
    // connection to this TA and connect to it.

    let child_name = request.uuid.to_string(); // TODO: This won't work for multi instance TAs
    let realm = connect_to_protocol::<fidl_fuchsia_component::RealmMarker>()
        .expect("connecting to Realm protocol");
    let child_decl = fidl_fuchsia_component_decl::Child {
        name: Some(child_name.to_string()),
        url: Some(config.url),
        startup: Some(fidl_fuchsia_component_decl::StartupMode::Eager),
        ..Default::default()
    };
    let (child_controller, child_controller_server) =
        fidl::endpoints::create_proxy().expect("creating child controller channel");
    let create_child_args = fidl_fuchsia_component::CreateChildArgs {
        controller: Some(child_controller_server),
        ..Default::default()
    };
    if let Err(e) = realm
        .create_child(
            &fidl_fuchsia_component_decl::CollectionRef { name: "ta".to_string() },
            &child_decl,
            create_child_args,
        )
        .await
    {
        tracing::error!("Could not create child component in TA collection: {e:?}");
        return;
    }

    let (ta_exposed_dir, ta_exposed_dir_server) =
        fidl::endpoints::create_proxy().expect("creating exposed directory channel");
    if let Err(e) = realm
        .open_exposed_dir(
            &fidl_fuchsia_component_decl::ChildRef {
                name: child_name,
                collection: Some("ta".to_string()),
            },
            ta_exposed_dir_server,
        )
        .await
    {
        tracing::error!("Could not open exposed directory on child component: {e:?}");
        return;
    }

    let ta =
        connect_to_protocol_at_dir_root::<fidl_fuchsia_tee::ApplicationMarker>(&ta_exposed_dir)
            .expect("connecting to Application protocol on child TA");

    use fidl_fuchsia_tee as ftee;
    fn overwrite_return_origin(mut result: ftee::OpResult) -> ftee::OpResult {
        result.return_origin = Some(ftee::ReturnOrigin::TrustedApplication);
        result
    }

    while let Some(request) = request.stream.next().await {
        match request {
            Ok(ftee::ApplicationRequest::OpenSession2 { parameter_set, responder }) => {
                if let Ok((code, result)) = ta.open_session2(parameter_set).await {
                    let _ = responder.send(code, overwrite_return_origin(result));
                }
            }
            Ok(ftee::ApplicationRequest::InvokeCommand {
                session_id,
                command_id,
                parameter_set,
                responder,
            }) => {
                if let Ok(result) = ta.invoke_command(session_id, command_id, parameter_set).await {
                    let _ = responder.send(overwrite_return_origin(result));
                }
            }
            Ok(ftee::ApplicationRequest::CloseSession { session_id, responder }) => {
                if let Ok(()) = ta.close_session(session_id).await {
                    let _ = responder.send();
                }
            }
            Err(_) => break,
        }
    }
    // TODO: We may need to keep the instance alive when we implement the singleInstance and
    // instanceKeepAlive properties. For now, explicitly drop the child controller when the client
    // disconnects.
    std::mem::drop(child_controller);
}

fn parse_config(path: &PathBuf) -> Result<TAConfig, Error> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Could not read config file at {path:?}: {e}"))?;
    let parsed = serde_json::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("Could not deserialize {path:?} from json: {e}"))?;
    Ok(parsed)
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut configs = HashMap::new();
    if let Ok(d) = std::fs::read_dir("/config") {
        let uuid_filenames = d
            .filter(|entry| entry.is_ok())
            .map(|entry| entry.unwrap().path())
            .filter(|path| !path.is_dir())
            .map(|entry| (entry.clone(), entry.file_name().unwrap().to_os_string()))
            .collect::<Vec<_>>();
        for (path, file) in uuid_filenames {
            let uuid = Path::new(&file)
                .file_stem()
                .ok_or(anyhow::anyhow!("Expected path with extension"))?;
            let config = parse_config(&path)?;
            let _ = configs.insert(
                uuid.to_str().ok_or(anyhow::anyhow!("UUID string did not decode"))?.to_string(),
                config,
            );
        }
    }

    let mut fs = ServiceFs::new_local();
    let mut svc_dir = fs.dir("svc");
    let mut ta_dir = svc_dir.dir("ta");

    for uuid in configs.keys().cloned() {
        let _ = ta_dir.dir(&uuid).add_fidl_service(move |stream: ApplicationRequestStream| {
            TAConnectRequest { uuid: uuid.to_string(), stream }
        });
    }

    let _ = fs.take_and_serve_directory_handle()?;

    let mut application_task_group = fasync::TaskGroup::new();

    while let Some(request) = fs.next().await {
        match configs.get(&request.uuid) {
            Some(config) => application_task_group
                .spawn(fasync::Task::spawn(run_application(request, config.clone()))),
            None => {
                tracing::warn!("Received connection request for unknown UUID {:?}", request.uuid)
            }
        }
    }

    application_task_group.join().await;

    Ok(())
}
