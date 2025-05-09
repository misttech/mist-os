// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Context};
use cm_logger::scoped::ScopedLogger;
use cm_rust::CapabilityTypeName;
use cm_types::{Name, NamespacePath};
use dispatcher_config::Config;
use fidl::endpoints::{DiscoverableProtocolMarker, ServerEnd};
use fuchsia_component::directory::AsRefDirectory;
use fuchsia_component::server;
use futures::{FutureExt, StreamExt};
use log::Log;
use namespace::Namespace;
use rand::Rng;
use sandbox::{Dict, Router};
use std::fmt::Debug;
use std::str::FromStr;
use std::sync::Arc;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_component_internal as finternal, fidl_fuchsia_component_sandbox as fsandbox,
    fidl_fuchsia_io as fio, fidl_fuchsia_logger as flogger,
};

fn scoped_log(scoped_logger: &ScopedLogger, error: anyhow::Error) {
    let mut builder = log::Record::builder();
    builder.level(log::Level::Warn);
    scoped_logger
        .log(&builder.args(format_args!("failed to run dispatcher component: {error}")).build());
}

/// The dispatcher is a built-in component that expects configuration capabilities giving it a
/// capability type, capability name, and component URL. It will expose a dictionary named "output"
/// that contains a capability of a given name and type. Any route requests that go to that
/// capability will cause the dispatcher to create a new dynamic child and forward the request to
/// the child under the same capability name. The new dynamic child will be offered everything that
/// is offered to the dispatcher, and the dispatcher will destroy the child when it stops running.
pub struct Dispatcher {
    realm_proxy: fcomponent::RealmProxy,
    capability_store_proxy: fsandbox::CapabilityStoreProxy,
    sandbox_retriever_proxy: finternal::ComponentSandboxRetrieverProxy,
    config: Arc<Config>,
    task_group: cm_util::TaskGroup,
}

impl Dispatcher {
    pub async fn run(
        mut namespace: Namespace,
        outgoing_dir: ServerEnd<fio::DirectoryMarker>,
        config: Option<zx::Vmo>,
    ) {
        let Some(svc_dir) = namespace.remove(&NamespacePath::new("/svc").unwrap()) else {
            log::error!("dispatcher is missing svc dir");
            return;
        };
        let svc_dir_proxy = svc_dir.into_proxy();
        let (log_sink_proxy, server) = fidl::endpoints::create_proxy::<flogger::LogSinkMarker>();
        if let Err(e) = svc_dir_proxy.as_ref_directory().open(
            flogger::LogSinkMarker::PROTOCOL_NAME,
            fio::Flags::PROTOCOL_SERVICE,
            server.into_channel().into(),
        ) {
            log::error!("failed to open logsink for dispatcher component: {e:?}");
            return;
        }
        let Ok(logger) = ScopedLogger::create(log_sink_proxy) else {
            log::error!("failed to create scoped logge for dispatcher component");
            return;
        };
        if let Err(err) = Self::run_inner(svc_dir_proxy, outgoing_dir, config, logger.clone()).await
        {
            scoped_log(&logger, err);
        }
    }

    pub async fn run_inner(
        svc_dir_proxy: fio::DirectoryProxy,
        outgoing_dir: ServerEnd<fio::DirectoryMarker>,
        config: Option<zx::Vmo>,
        logger: ScopedLogger,
    ) -> Result<(), anyhow::Error> {
        let (realm_proxy, realm_server_end) =
            fidl::endpoints::create_proxy::<fcomponent::RealmMarker>();
        svc_dir_proxy
            .as_ref_directory()
            .open(
                fcomponent::RealmMarker::PROTOCOL_NAME,
                fio::Flags::PROTOCOL_SERVICE,
                realm_server_end.into_channel().into(),
            )
            .context("failed to open protocol")?;
        let (capability_store_proxy, capability_store_server_end) =
            fidl::endpoints::create_proxy::<fsandbox::CapabilityStoreMarker>();
        svc_dir_proxy
            .as_ref_directory()
            .open(
                fsandbox::CapabilityStoreMarker::PROTOCOL_NAME,
                fio::Flags::PROTOCOL_SERVICE,
                capability_store_server_end.into_channel().into(),
            )
            .context("failed to open protocol")?;
        let (sandbox_retriever_proxy, sandbox_retriever_server_end) =
            fidl::endpoints::create_proxy::<finternal::ComponentSandboxRetrieverMarker>();
        svc_dir_proxy
            .as_ref_directory()
            .open(
                finternal::ComponentSandboxRetrieverMarker::PROTOCOL_NAME,
                fio::Flags::PROTOCOL_SERVICE,
                sandbox_retriever_server_end.into_channel().into(),
            )
            .context("failed to open protocol")?;

        let config = Arc::new(
            Config::from_vmo(&config.context("dispatcher is missing config vmo")?)
                .context("failed to parse config")?,
        );

        let self_ = Arc::new(Self {
            realm_proxy,
            capability_store_proxy,
            sandbox_retriever_proxy,
            config,
            task_group: cm_util::TaskGroup::new(),
        });

        let mut fs = server::ServiceFs::new();
        fs.dir("svc").add_fidl_service(move |stream: fsandbox::DictionaryRouterRequestStream| {
            let self_clone = self_.clone();
            let logger = logger.clone();
            self_.task_group.spawn(async move {
                if let Err(err) = self_clone.handle_router_stream(stream).await {
                    scoped_log(&logger, err);
                }
            });
        });
        fs.serve_connection(outgoing_dir).context("failed to serve outgoing directory")?;
        fs.collect::<()>().await;
        Ok(())
    }

    async fn handle_router_stream(
        self: Arc<Self>,
        mut stream: fsandbox::DictionaryRouterRequestStream,
    ) -> Result<(), anyhow::Error> {
        while let Some(Ok(request)) = stream.next().await {
            match request {
                fsandbox::DictionaryRouterRequest::Route { payload: _, responder } => {
                    let return_value = Dict::new();
                    let self_clone = self.clone();
                    let cap = match CapabilityTypeName::from_str(&self.config.type_to_dispatch) {
                        Ok(CapabilityTypeName::EventStream)
                        | Ok(CapabilityTypeName::Resolver)
                        | Ok(CapabilityTypeName::Runner)
                        | Ok(CapabilityTypeName::Protocol) => {
                            Router::new(move |request, debug: bool| {
                                assert!(!debug);
                                self_clone.clone().route::<sandbox::Connector>(request).boxed()
                            })
                            .into()
                        }
                        Ok(CapabilityTypeName::Directory) | Ok(CapabilityTypeName::Storage) => {
                            Router::new(move |request, debug: bool| {
                                assert!(!debug);
                                self_clone.clone().route::<sandbox::DirConnector>(request).boxed()
                            })
                            .into()
                        }
                        Ok(CapabilityTypeName::Service) => {
                            Router::new(move |request, debug: bool| {
                                assert!(!debug);
                                self_clone.clone().route::<sandbox::DirEntry>(request).boxed()
                            })
                            .into()
                        }
                        Ok(CapabilityTypeName::Dictionary) => {
                            Router::new(move |request, debug: bool| {
                                assert!(!debug);
                                self_clone.clone().route::<sandbox::Dict>(request).boxed()
                            })
                            .into()
                        }
                        Ok(CapabilityTypeName::Config) => {
                            Router::new(move |request, debug: bool| {
                                assert!(!debug);
                                self_clone.clone().route::<sandbox::Data>(request).boxed()
                            })
                            .into()
                        }
                        Err(_parse_err) => {
                            return Err(format_err!(
                                "unrecognized capability type {:?}",
                                self.config.type_to_dispatch
                            ));
                        }
                    };
                    return_value
                        .insert(
                            Name::new(&self.config.what_to_dispatch).expect("invalid name"),
                            cap,
                        )
                        .expect("failed to insert into dictionary");
                    let _ = responder.send(Ok(
                        fsandbox::DictionaryRouterRouteResponse::Dictionary(return_value.into()),
                    ));
                }
                other_request => return Err(format_err!("unexpected request: {other_request:?}")),
            }
        }
        Ok(())
    }

    async fn route<T>(
        self: Arc<Self>,
        request: Option<sandbox::Request>,
    ) -> Result<sandbox::RouterResponse<T>, router_error::RouterError>
    where
        T: sandbox::CapabilityBound + Debug,
        Router<T>: TryFrom<sandbox::Capability> + Debug,
        <sandbox::Router<T> as std::convert::TryFrom<sandbox::Capability>>::Error: Debug,
    {
        match self.get_target_router().await {
            Ok(router_to_dispatch_to) => router_to_dispatch_to.route(request, false).await,
            Err(err) => {
                log::warn!("failed: {err:?}");
                Err(router_error::RouterError::Internal)
            }
        }
    }

    async fn get_target_router<T>(self: Arc<Self>) -> Result<sandbox::Router<T>, anyhow::Error>
    where
        T: sandbox::CapabilityBound + Debug,
        Router<T>: TryFrom<sandbox::Capability> + Debug,
        <sandbox::Router<T> as std::convert::TryFrom<sandbox::Capability>>::Error: Debug,
    {
        let component_input_capabilities = Self::get_component_input_capabilities(
            &self.capability_store_proxy,
            &self.sandbox_retriever_proxy,
        )
        .await?;
        let child_name = format!("worker-{:x}", rand::thread_rng().gen::<u64>());
        let (controller_proxy, controller_server_end) =
            fidl::endpoints::create_proxy::<fcomponent::ControllerMarker>();
        self.realm_proxy
            .create_child(
                &fdecl::CollectionRef { name: "workers".to_string() },
                &fdecl::Child {
                    name: Some(child_name.clone()),
                    url: Some(self.config.who_to_dispatch_to.clone()),
                    startup: Some(fdecl::StartupMode::Lazy),
                    ..Default::default()
                },
                fcomponent::CreateChildArgs {
                    controller: Some(controller_server_end),
                    dictionary: Some(component_input_capabilities),
                    ..Default::default()
                },
            )
            .await
            .context("FIDL error talking to ourselves")?
            .map_err(|e| format_err!("failed to create child: {e:?}"))?;
        self.clone().spawn_wait_for_exit(controller_proxy).await;
        let child_output_dictionary: sandbox::Dict = self
            .realm_proxy
            .get_child_output_dictionary(&fdecl::ChildRef {
                name: child_name.clone(),
                collection: Some("workers".to_string()),
            })
            .await
            .context("FIDL error talking to ourselves")?
            .map_err(|e| format_err!("failed to get output dictionary: {e:?}"))?
            .try_into()
            .unwrap();
        let capability_to_dispatch_to = child_output_dictionary
            .get(&Name::new(&self.config.what_to_dispatch).expect("invalid name"))
            .expect("child output dictionary holds un-cloneable item")
            .context(format!("child doesn't expose {:?}", &self.config.what_to_dispatch))?;
        let router_to_dispatch_to: sandbox::Router<T> =
            sandbox::Router::try_from(capability_to_dispatch_to)
                .map_err(|e| format_err!("child exposes incorrect router type: {e:?}"))?;
        Ok(router_to_dispatch_to)
    }

    async fn get_component_input_capabilities(
        capability_store_proxy: &fsandbox::CapabilityStoreProxy,
        sandbox_retriever_proxy: &finternal::ComponentSandboxRetrieverProxy,
    ) -> Result<fsandbox::DictionaryRef, anyhow::Error> {
        let sandbox = sandbox_retriever_proxy
            .get_my_sandbox()
            .await
            .context("failed to use framework protocol from built-in component")?;
        let parent_id = rand::thread_rng().gen::<u64>();
        let child_id = rand::thread_rng().gen::<u64>();
        let child_clone_id = rand::thread_rng().gen::<u64>();
        capability_store_proxy
            .import(parent_id, fsandbox::Capability::Dictionary(sandbox.component_input.unwrap()))
            .await
            .context("FIDL error talking to ourselves")?
            .map_err(|e| format_err!("invalid dictionary reference: {e:?}"))?;
        capability_store_proxy
            .dictionary_get(parent_id, "parent", child_id)
            .await
            .context("FIDL error talking to ourselves")?
            .map_err(|e| format_err!("component input missing capabilities directory: {e:?}"))?;
        capability_store_proxy
            .dictionary_copy(child_id, child_clone_id)
            .await
            .context("FIDL error talking to ourselves")?
            .map_err(|e| format_err!("failed to copy dictionary: {e:?}"))?;
        let _ = capability_store_proxy.dictionary_remove(child_clone_id, "diagnostics", None).await;
        let fsandbox::Capability::Dictionary(capabilities_dictionary_ref) = capability_store_proxy
            .export(child_clone_id)
            .await
            .context("FIDL error talking to ourselves")?
            .map_err(|e| format_err!("failed to export capability: {e:?}"))?
        else {
            return Err(format_err!("wrong capability type returned by store"));
        };
        Ok(capabilities_dictionary_ref)
    }

    async fn spawn_wait_for_exit(self: Arc<Self>, controller_proxy: fcomponent::ControllerProxy) {
        let (execution_controller_proxy, execution_controller_server_end) =
            fidl::endpoints::create_proxy::<fcomponent::ExecutionControllerMarker>();
        controller_proxy
            .start(fcomponent::StartChildArgs::default(), execution_controller_server_end)
            .await
            .expect("FIDL error talking to ourselves")
            .expect("failed to start child");
        self.task_group.spawn(async move {
            let mut event_stream = execution_controller_proxy.take_event_stream();
            match event_stream.next().await {
                Some(Ok(fcomponent::ExecutionControllerEvent::OnStop { .. })) => {
                    controller_proxy
                        .destroy()
                        .await
                        .expect("FIDL error talking to ourselves")
                        .expect("failed to destroy child");
                }
                event => panic!("unexpected execution controller event: {:?}", event),
            }
        });
    }
}
