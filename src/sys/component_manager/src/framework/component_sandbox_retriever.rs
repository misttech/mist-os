// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::model::component::WeakComponentInstance;
use ::routing::bedrock::sandbox_construction::{ComponentSandbox, ProgramInput};
use ::routing::bedrock::structured_dict::ComponentInput;
use ::routing::capability_source::CapabilitySource;
use anyhow::{format_err, Error};
use cm_types::Name;
use fidl::endpoints::ServerEnd;
use fidl_fuchsia_component_internal as finternal;
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use sandbox::{Dict, RouterResponse};

pub fn serve(
    chan: zx::Channel,
    _target: WeakComponentInstance,
    source: WeakComponentInstance,
) -> BoxFuture<'static, Result<(), Error>> {
    async move {
        let mut stream =
            ServerEnd::<finternal::ComponentSandboxRetrieverMarker>::new(chan).into_stream();
        while let Some(Ok(request)) = stream.next().await {
            match request {
                finternal::ComponentSandboxRetrieverRequest::GetMySandbox { responder } => {
                    let ComponentSandbox {
                        component_input,
                        component_output,
                        program_input,
                        program_output_dict,
                        capability_sourced_capabilities_dict,
                        declared_dictionaries,
                        child_inputs,
                        collection_inputs,
                        ..
                    } = source
                        .upgrade()
                        .map_err(|e| format_err!("failed to upgrade component: {:?}", e))?
                        .lock_resolved_state()
                        .await
                        .map_err(|e| format_err!("failed to resolve component: {:?}", e))?
                        .sandbox
                        .clone();
                    if !is_builtin_runner(&program_input).await {
                        // This API is explerimental and making it widely available has security
                        // implications. To allow us to get a bit of mileage on it to determine the
                        // correct shape for it, we currently only allow connections to this
                        // protocol if the client is a built-in component.
                        return Err(format_err!("only accessible from built-in components"));
                    }
                    let to_fidl = |(name, input): (Name, ComponentInput)| finternal::ChildInput {
                        child_name: name.to_string(),
                        child_input: Dict::from(input).into(),
                    };
                    responder.send(finternal::ComponentSandbox {
                        component_input: Some(Dict::from(component_input).into()),
                        component_output: Some(Dict::from(component_output).into()),
                        program_input: Some(Dict::from(program_input).into()),
                        program_output: Some(program_output_dict.into()),
                        capability_sourced: Some(capability_sourced_capabilities_dict.into()),
                        declared_dictionaries: Some(declared_dictionaries.into()),
                        child_inputs: Some(child_inputs.enumerate().map(to_fidl).collect()),
                        collection_inputs: Some(
                            collection_inputs.enumerate().map(to_fidl).collect(),
                        ),
                        ..Default::default()
                    })?;
                }
                ord => {
                    return Err(format_err!("unrecognized ordinal: {:?}", ord));
                }
            }
        }
        Ok(())
    }
    .boxed()
}

async fn is_builtin_runner(program_input: &ProgramInput) -> bool {
    let Some(runner_router) = program_input.runner() else {
        return false;
    };
    let Ok(RouterResponse::Debug(source_data)) = runner_router.route(None, true).await else {
        return false;
    };
    let source: ::routing::capability_source::CapabilitySource =
        source_data.try_into().expect("failed to convert into capability source");
    let CapabilitySource::Builtin(_) = source else {
        return false;
    };
    true
}

#[cfg(all(test, not(feature = "src_model_tests")))]
mod tests {
    use super::*;
    use crate::model::testing::test_helpers::*;
    use crate::model::testing::test_hook::*;
    use ::routing::component_instance::ComponentInstanceInterface;
    use cm_config::RuntimeConfig;
    use cm_rust::{ComponentDecl, ExposeSource};
    use cm_rust_testing::*;
    use fuchsia_async as fasync;
    use std::sync::Arc;

    async fn get_sandbox(
        components: Vec<(&'static str, ComponentDecl)>,
    ) -> Option<finternal::ComponentSandbox> {
        let config = RuntimeConfig { list_children_batch_size: 2, ..Default::default() };
        let hook = Arc::new(TestHook::new());
        let test = TestEnvironmentBuilder::new()
            .set_runtime_config(config)
            .set_components(components)
            .set_front_hooks(hook.hooks())
            .build()
            .await;

        // Look up and start component.
        let component = test.model.root().clone();

        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<finternal::ComponentSandboxRetrieverMarker>();
        let _serving_task = fasync::Task::spawn(async move {
            if let Err(e) =
                serve(server_end.into_channel(), component.as_weak(), component.as_weak()).await
            {
                log::warn!("failure in serve function: {e:?}");
            }
        });
        proxy.get_my_sandbox().await.ok()
    }

    #[fuchsia::test]
    async fn builtin_runner() {
        // ComponentDeclBuilder::new() automatically sets our runner to `test_runner`, which is a
        // built-in runner
        let components = vec![("root", ComponentDeclBuilder::new().build())];
        assert!(get_sandbox(components).await.is_some());
    }

    #[fuchsia::test]
    async fn invalid_runner() {
        let components = vec![(
            "root",
            ComponentDeclBuilder::new_empty_component().program_runner("nonexistent").build(),
        )];
        assert!(get_sandbox(components).await.is_none());
    }

    #[fuchsia::test]
    async fn non_builtin_runner() {
        let components = vec![
            (
                "root",
                ComponentDeclBuilder::new_empty_component()
                    .child_default("child")
                    .use_(UseBuilder::runner().name("example_runner").source_static_child("child"))
                    .build(),
            ),
            (
                "child",
                ComponentDeclBuilder::new()
                    .capability(CapabilityBuilder::runner().name("example_runner"))
                    .expose(
                        ExposeBuilder::runner().name("example_runner").source(ExposeSource::Self_),
                    )
                    .build(),
            ),
        ];
        assert!(get_sandbox(components).await.is_none());
    }

    #[fuchsia::test]
    async fn no_runner() {
        let components = vec![("root", ComponentDeclBuilder::new_empty_component().build())];
        assert!(get_sandbox(components).await.is_none());
    }
}
