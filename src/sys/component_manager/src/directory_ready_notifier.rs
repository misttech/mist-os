// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::model::{
        component::{ComponentInstance, InstanceState},
        error::ModelError,
        events::synthesizer::{EventSynthesisProvider, ExtendedComponent},
        hooks::{Event, EventPayload, EventType, Hook, HooksRegistration},
        model::Model,
    },
    ::routing::{event::EventFilter, rights::Rights},
    async_trait::async_trait,
    cm_rust::{ComponentDecl, ExposeDecl, ExposeDirectoryDecl, ExposeSource, ExposeTarget},
    cm_types::{Name, Path},
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fuchsia_async as fasync, fuchsia_zircon as zx,
    futures::stream::StreamExt,
    moniker::Moniker,
    std::sync::{Arc, Weak},
    thiserror::Error,
};

/// Awaits for `Started` events and for each capability exposed to framework, dispatches a
/// `DirectoryReady` event.
pub struct DirectoryReadyNotifier {
    model: Weak<Model>,
}

impl DirectoryReadyNotifier {
    pub fn new(model: Weak<Model>) -> Self {
        Self { model }
    }

    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![HooksRegistration::new(
            "DirectoryReadyNotifier",
            vec![EventType::Started],
            Arc::downgrade(self) as Weak<dyn Hook>,
        )]
    }

    async fn on_component_started(
        self: &Arc<Self>,
        target_moniker: &Moniker,
        outgoing_dir: &fio::DirectoryProxy,
        decl: ComponentDecl,
    ) -> Result<(), ModelError> {
        // Don't block the handling on the event on the exposed capabilities being ready
        let this = self.clone();
        let target_moniker = target_moniker.clone();
        let outgoing_dir = Clone::clone(outgoing_dir);
        fasync::Task::spawn(async move {
            // If we can't find the component then we can't dispatch any DirectoryReady event,
            // error or otherwise. This isn't necessarily an error as the model or component might've been
            // destroyed in the intervening time, so we just exit early.
            let target = match this.model.upgrade() {
                Some(model) => {
                    if let Ok(component) =
                        model.root().find_and_maybe_resolve(&target_moniker).await
                    {
                        component
                    } else {
                        return;
                    }
                }
                None => return,
            };

            let matching_exposes = filter_matching_exposes(&decl, None);
            this.dispatch_capabilities_ready(outgoing_dir, &decl, matching_exposes, &target).await;
        })
        .detach();
        Ok(())
    }

    /// Waits for the outgoing directory to be ready and then notifies hooks of all the capabilities
    /// inside it that were exposed to the framework by the component.
    async fn dispatch_capabilities_ready(
        &self,
        outgoing_dir: fio::DirectoryProxy,
        decl: &ComponentDecl,
        matching_exposes: Vec<&ExposeDecl>,
        target: &Arc<ComponentInstance>,
    ) {
        let directory_ready_events =
            self.create_events(Some(outgoing_dir), decl, matching_exposes, target).await;
        for directory_ready_event in directory_ready_events {
            target.hooks.dispatch(&directory_ready_event).await;
        }
    }

    async fn create_events(
        &self,
        outgoing_dir: Option<fio::DirectoryProxy>,
        decl: &ComponentDecl,
        matching_exposes: Vec<&ExposeDecl>,
        target: &Arc<ComponentInstance>,
    ) -> Vec<Event> {
        // Forward along the result for opening the outgoing directory into the DirectoryReady
        // dispatch in order to propagate any potential errors as an event.
        let outgoing_dir_result = async move {
            let outgoing_dir = outgoing_dir?;

            let (outgoing_dir_proxy, server_end) =
                fidl::endpoints::create_proxy::<fio::DirectoryMarker>().ok()?;
            _ = outgoing_dir.open(
                fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DESCRIBE,
                fio::ModeType::empty(),
                ".",
                server_end.into_channel().into(),
            );
            let mut events = outgoing_dir_proxy.take_event_stream();

            let () = match events.next().await {
                Some(Ok(fio::DirectoryEvent::OnOpen_ { s: status, info: _ })) => {
                    zx::Status::ok(status).ok()
                }
                Some(Ok(fio::DirectoryEvent::OnRepresentation { .. })) => Some(()),
                _ => None,
            }?;
            Some(outgoing_dir_proxy)
        }
        .await
        .ok_or_else(|| ModelError::open_directory_error(target.moniker.clone(), "/"));

        let mut events = Vec::new();
        for expose_decl in matching_exposes {
            let event = match expose_decl {
                ExposeDecl::Directory(ExposeDirectoryDecl { source_name, target_name, .. }) => {
                    let (source_path, rights) = {
                        if let Some(directory_decl) = decl.find_directory_source(source_name) {
                            (
                                directory_decl
                                    .source_path
                                    .as_ref()
                                    .expect("missing directory source path"),
                                directory_decl.rights,
                            )
                        } else {
                            panic!("Missing directory declaration for expose: {:?}", decl);
                        }
                    };
                    self.create_event(
                        &target,
                        outgoing_dir_result.as_ref(),
                        Rights::from(rights),
                        source_path,
                        target_name,
                    )
                    .await
                }
                _ => {
                    unreachable!("should have skipped above");
                }
            };
            if let Some(event) = event {
                events.push(event);
            }
        }

        events
    }

    /// Creates an event with the directory at the given `target_path` inside the provided
    /// outgoing directory if the capability is available.
    async fn create_event(
        &self,
        target: &Arc<ComponentInstance>,
        outgoing_dir_result: Result<&fio::DirectoryProxy, &ModelError>,
        rights: Rights,
        source_path: &Path,
        target_name: &Name,
    ) -> Option<Event> {
        let target_name = target_name.to_string();
        // DirProxy.open fails on absolute paths.
        let source_path = source_path.to_string();
        let Ok(outgoing_dir) = outgoing_dir_result.map_err(|e| e.clone()) else {
            return None;
        };
        self.try_opening(&outgoing_dir, &source_path, &rights)
            .await
            .map(|node| {
                Event::new(&target, EventPayload::DirectoryReady { name: target_name, node })
            })
            .ok()
    }

    async fn try_opening(
        &self,
        outgoing_dir: &fio::DirectoryProxy,
        source_path: &str,
        rights: &Rights,
    ) -> Result<fio::NodeProxy, TryOpenError> {
        // Check that the directory is present first.
        let canonicalized_path = fuchsia_fs::canonicalize_path(&source_path);
        if !fuchsia_fs::directory::dir_contains(&outgoing_dir, canonicalized_path).await? {
            return Err(TryOpenError::Status(zx::Status::NOT_FOUND));
        }
        let (node, server_end) = fidl::endpoints::create_proxy::<fio::NodeMarker>().unwrap();
        outgoing_dir.open(
            rights.into_legacy() | fio::OpenFlags::DESCRIBE | fio::OpenFlags::DIRECTORY,
            fio::ModeType::empty(),
            &canonicalized_path,
            ServerEnd::new(server_end.into_channel()),
        )?;
        let mut events = node.take_event_stream();
        match events.next().await {
            Some(Ok(fio::NodeEvent::OnOpen_ { s: status, .. })) => {
                let zx_status = zx::Status::from_raw(status);
                if zx_status != zx::Status::OK {
                    return Err(TryOpenError::Status(zx_status));
                }
            }
            Some(Ok(fio::NodeEvent::OnRepresentation { .. })) => {}
            _ => {
                return Err(TryOpenError::Status(zx::Status::PEER_CLOSED));
            }
        }
        Ok(node)
    }
}

fn filter_matching_exposes<'a>(
    decl: &'a ComponentDecl,
    filter: Option<&EventFilter>,
) -> Vec<&'a ExposeDecl> {
    decl.exposes
        .iter()
        .filter(|expose_decl| {
            match expose_decl {
                ExposeDecl::Directory(ExposeDirectoryDecl {
                    source, target, target_name, ..
                }) => {
                    if let Some(filter) = filter {
                        if !filter.contains("name", vec![target_name.to_string()]) {
                            return false;
                        }
                    }
                    if target != &ExposeTarget::Framework || source != &ExposeSource::Self_ {
                        return false;
                    }
                }
                _ => {
                    return false;
                }
            }
            true
        })
        .collect()
}

#[async_trait]
impl EventSynthesisProvider for DirectoryReadyNotifier {
    async fn provide(&self, component: ExtendedComponent, filter: &EventFilter) -> Vec<Event> {
        let component = match component {
            ExtendedComponent::ComponentInstance(component) => component,
            ExtendedComponent::ComponentManager => {
                return vec![];
            }
        };
        let decl = match *component.lock_state().await {
            InstanceState::Resolved(ref s) => s.decl().clone(),
            InstanceState::New | InstanceState::Unresolved(_) | InstanceState::Destroyed => {
                return vec![];
            }
        };
        let matching_exposes = filter_matching_exposes(&decl, Some(&filter));
        if matching_exposes.is_empty() {
            // Short-circuit if there are no matching exposes so we don't wait for the component's
            // outgoing directory if there are no DirectoryReady events to send.
            return vec![];
        }

        let outgoing_dir = {
            let execution = component.lock_execution().await;
            match execution.runtime.as_ref() {
                Some(runtime) => runtime.outgoing_dir().cloned(),
                None => return vec![],
            }
        };
        self.create_events(outgoing_dir, &decl, matching_exposes, &component).await
    }
}

#[derive(Error, Debug)]
enum TryOpenError {
    #[error(transparent)]
    Fidl(#[from] fidl::Error),
    #[error(transparent)]
    Status(#[from] zx::Status),
    #[error(transparent)]
    Enumerate(#[from] fuchsia_fs::directory::EnumerateError),
}

#[async_trait]
impl Hook for DirectoryReadyNotifier {
    async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
        let target_moniker = event
            .target_moniker
            .unwrap_instance_moniker_or(ModelError::UnexpectedComponentManagerMoniker)?;
        match &event.payload {
            EventPayload::Started { runtime, component_decl, .. } => {
                if filter_matching_exposes(&component_decl, None).is_empty() {
                    // Short-circuit if there are no matching exposes so we don't spawn a task
                    // if there's nothing to do. In particular, don't wait for the component's
                    // outgoing directory if there are no DirectoryReady events to send.
                    return Ok(());
                }
                if let Some(outgoing_dir) = &runtime.outgoing_dir {
                    self.on_component_started(
                        &target_moniker,
                        outgoing_dir,
                        component_decl.clone(),
                    )
                    .await?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
