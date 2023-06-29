// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    cm_rust::FidlIntoNative,
    fidl::endpoints::{create_endpoints, create_proxy, Proxy},
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fcdecl,
    fidl_fuchsia_io as fio, fidl_fuchsia_process as fprocess, fuchsia_async as fasync,
    fuchsia_component_test::{
        Capability, ChildOptions, LocalComponentHandles, RealmBuilder, RealmInstance, Ref, Route,
    },
    fuchsia_runtime::{HandleInfo, HandleType},
    fuchsia_zircon::{self as zx, AsHandleRef, HandleBased},
    futures::{
        channel::{mpsc, oneshot},
        FutureExt, SinkExt, StreamExt, TryStreamExt,
    },
    std::sync::{Arc, Mutex},
    vfs::{
        directory::entry::DirectoryEntry as _, execution_scope::ExecutionScope,
        file::vmo::read_only, pseudo_directory,
    },
};

const COLLECTION_NAME: &'static str = "col";

/// Marks the `component_status` as not running when dropped.
struct DropMarksComponentAsStopped {
    component_status: ComponentStatus,
}

impl Drop for DropMarksComponentAsStopped {
    fn drop(&mut self) {
        let mut is_running_guard = self.component_status.is_running.lock().unwrap();
        if !*is_running_guard {
            panic!("component was already stopped");
        }
        assert!(*is_running_guard, "component is already stopped");
        *is_running_guard = false;
    }
}

/// Tracks if a local component is running. The local component calls `new_run` when it starts
/// which marks it as running, and when the struct returned by that function is dropped the
/// component is marked as not running.
#[derive(Clone, Default)]
struct ComponentStatus {
    // We use std::sync::Mutex because we need to unlock it in a drop function, which is not async
    is_running: Arc<Mutex<bool>>,
}

impl ComponentStatus {
    fn is_running(&self) -> bool {
        *self.is_running.lock().unwrap()
    }

    fn new_run(&self) -> DropMarksComponentAsStopped {
        let mut is_running_guard = self.is_running.lock().unwrap();
        assert!(!*is_running_guard, "component is already running");
        *is_running_guard = true;
        DropMarksComponentAsStopped { component_status: self.clone() }
    }
}

/// Creates a nested component manager instance with a collection, and invokes the `create_child`
/// call to create a child within that collection.
async fn launch_child_in_a_collection_in_nested_component_manager(
    collection_ref: fcdecl::CollectionRef,
    child_decl: fcdecl::Child,
    child_args: fcomponent::CreateChildArgs,
) -> RealmInstance {
    let builder = RealmBuilder::new().await.unwrap();
    let child_args = Arc::new(Mutex::new(Some(child_args)));
    let realm_user = builder
        .add_local_child(
            "realm_user",
            move |handles| {
                let collection_ref = collection_ref.clone();
                let child_decl = child_decl.clone();
                let child_args = child_args.clone();
                async move {
                    let realm_proxy =
                        handles.connect_to_protocol::<fcomponent::RealmMarker>().unwrap();
                    let child_args = {
                        let mut child_args_guard = child_args.lock().unwrap();
                        child_args_guard.take().unwrap()
                    };
                    realm_proxy
                        .create_child(&collection_ref, &child_decl, child_args)
                        .await
                        .unwrap()
                        .unwrap();
                    Ok(())
                }
                .boxed()
            },
            ChildOptions::new().eager(),
        )
        .await
        .unwrap();
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<fcomponent::RealmMarker>())
                .from(Ref::framework())
                .to(&realm_user),
        )
        .await
        .unwrap();
    let mut realm_decl = builder.get_realm_decl().await.unwrap();
    realm_decl.collections.push(
        fcdecl::Collection {
            name: Some(COLLECTION_NAME.to_string()),
            durability: Some(fcdecl::Durability::Transient),
            ..Default::default()
        }
        .fidl_into_native(),
    );
    builder.replace_realm_decl(realm_decl).await.unwrap();
    builder.build_in_nested_component_manager("#meta/component_manager.cm").await.unwrap()
}

/// Represents a local child that was created in a nested realm builder.
struct SpawnedChild {
    /// The task which executes the local child.
    _local_child_task: fasync::Task<()>,
    /// The nested component manager instance that is hosting the local child.
    _cm_realm_instance: RealmInstance,
    /// When the local child is started it will send its handles over this mpsc.
    handles_receiver: mpsc::UnboundedReceiver<LocalComponentHandles>,
    /// The `fuchsia.component.Controller` channel that was created for the child.
    controller_proxy: fcomponent::ControllerProxy,
    /// Tracks when the component is and is not running.
    component_status: ComponentStatus,
    /// A oneshot that can be used to instruct the local component to exit.
    cancel_sender: Option<oneshot::Sender<()>>,
}

/// Spawns a local child and populates the `SpawnedChild` struct.
async fn spawn_local_child() -> SpawnedChild {
    let (handles_sender, handles_receiver) = mpsc::unbounded();
    let (cancel_sender, cancel_receiver) = oneshot::channel();
    let builder = RealmBuilder::new().await.unwrap();
    let component_status = ComponentStatus::default();
    let component_status_clone = component_status.clone();
    let cancel_receiver = Arc::new(Mutex::new(Some(cancel_receiver)));
    let child_ref = builder
        .add_local_child(
            "child",
            move |handles| {
                let mut handles_sender = handles_sender.clone();
                let component_status_clone = component_status_clone.clone();
                let cancel_receiver = cancel_receiver.clone();
                async move {
                    let _drop_marks_component_as_stopped = component_status_clone.new_run();
                    handles_sender.send(handles).await.unwrap();
                    let cancel_receiver = cancel_receiver.lock().unwrap().take().unwrap();
                    let _ = cancel_receiver.await;
                    Ok(())
                }
                .boxed()
            },
            ChildOptions::new(),
        )
        .await
        .unwrap();
    let child_decl = builder.get_component_decl(&child_ref).await.unwrap();
    builder.replace_realm_decl(child_decl).await.unwrap();
    let (url, local_child_task) = builder.initialize().await.unwrap();

    let collection_ref = fcdecl::CollectionRef { name: COLLECTION_NAME.to_string() };
    let child_decl = fcdecl::Child {
        name: Some("local_child".to_string()),
        url: Some(url),
        startup: Some(fcdecl::StartupMode::Lazy),
        ..Default::default()
    };
    let (controller_proxy, server_end) = create_proxy::<fcomponent::ControllerMarker>().unwrap();
    let child_args =
        fcomponent::CreateChildArgs { controller: Some(server_end), ..Default::default() };
    let cm_realm_instance = launch_child_in_a_collection_in_nested_component_manager(
        collection_ref,
        child_decl,
        child_args,
    )
    .await;
    SpawnedChild {
        _local_child_task: local_child_task,
        _cm_realm_instance: cm_realm_instance,
        handles_receiver,
        controller_proxy,
        component_status,
        cancel_sender: Some(cancel_sender),
    }
}

#[fuchsia::test]
pub async fn start() {
    let mut spawned_child = spawn_local_child().await;
    let (_execution_controller_proxy, execution_controller_server_end) =
        create_proxy::<fcomponent::ExecutionControllerMarker>().unwrap();
    spawned_child
        .controller_proxy
        .start(Default::default(), execution_controller_server_end)
        .await
        .unwrap()
        .unwrap();
    assert!(
        spawned_child.handles_receiver.next().await.is_some(),
        "failed to observe the local child be started"
    );
    assert!(spawned_child.component_status.is_running());
}

#[fuchsia::test]
pub async fn start_with_namespace_entries() {
    let mut spawned_child = spawn_local_child().await;
    let (ns_client_end, ns_server_end) = create_endpoints::<fio::DirectoryMarker>();
    let (_execution_controller_proxy, execution_controller_server_end) =
        create_proxy::<fcomponent::ExecutionControllerMarker>().unwrap();
    spawned_child
        .controller_proxy
        .start(
            fcomponent::StartChildArgs {
                namespace_entries: Some(vec![fcomponent::NamespaceEntry {
                    path: Some("/test".to_string()),
                    directory: Some(ns_client_end),
                    ..Default::default()
                }]),
                ..Default::default()
            },
            execution_controller_server_end,
        )
        .await
        .unwrap()
        .unwrap();

    let local_component_handles = spawned_child.handles_receiver.next().await.unwrap();
    let test_dir_proxy = local_component_handles.clone_from_namespace("test").unwrap();

    let () = pseudo_directory! {
        "file.txt" => read_only("hippos"),
    }
    .open(
        ExecutionScope::new(),
        fuchsia_fs::OpenFlags::RIGHT_READABLE,
        vfs::path::Path::dot(),
        ns_server_end.into_channel().into(),
    );

    let file_proxy = fuchsia_fs::directory::open_file(
        &test_dir_proxy,
        "file.txt",
        fio::OpenFlags::RIGHT_READABLE,
    )
    .await
    .unwrap();
    let file_contents = fuchsia_fs::file::read_to_string(&file_proxy).await.unwrap();
    assert_eq!(file_contents, "hippos".to_string());
}

#[fuchsia::test]
pub async fn start_with_numbered_handles() {
    let mut spawned_child = spawn_local_child().await;
    let handle_id = HandleInfo::new(HandleType::User0, 0).as_raw();
    let (s1, s2) = zx::Socket::create_stream();
    let (_execution_controller_proxy, execution_controller_server_end) =
        create_proxy::<fcomponent::ExecutionControllerMarker>().unwrap();
    spawned_child
        .controller_proxy
        .start(
            fcomponent::StartChildArgs {
                numbered_handles: Some(vec![fprocess::HandleInfo {
                    id: handle_id,
                    handle: s2.into_handle(),
                }]),
                ..Default::default()
            },
            execution_controller_server_end,
        )
        .await
        .unwrap()
        .unwrap();

    let mut local_component_handles = spawned_child.handles_receiver.next().await.unwrap();
    let s2_from_runner = local_component_handles
        .take_numbered_handle(handle_id)
        .expect("child was not given the numbered handle");
    assert_eq!(s1.basic_info().unwrap().related_koid, s2_from_runner.get_koid().unwrap());
}

#[fuchsia::test]
pub async fn channel_is_closed_on_stop() {
    let mut spawned_child = spawn_local_child().await;
    let (execution_controller_proxy, execution_controller_server_end) =
        create_proxy::<fcomponent::ExecutionControllerMarker>().unwrap();
    spawned_child
        .controller_proxy
        .start(Default::default(), execution_controller_server_end)
        .await
        .unwrap()
        .unwrap();
    assert!(
        spawned_child.handles_receiver.next().await.is_some(),
        "failed to observe the local child be started"
    );
    assert!(spawned_child.component_status.is_running());
    execution_controller_proxy.stop().unwrap();
    let execution_controller_channel = execution_controller_proxy.into_channel().unwrap();
    fasync::OnSignals::new(&execution_controller_channel, zx::Signals::CHANNEL_PEER_CLOSED)
        .await
        .unwrap();
    assert!(!spawned_child.component_status.is_running());
}

#[fuchsia::test]
pub async fn on_stop_is_called() {
    let mut spawned_child = spawn_local_child().await;
    let (execution_controller_proxy, execution_controller_server_end) =
        create_proxy::<fcomponent::ExecutionControllerMarker>().unwrap();
    spawned_child
        .controller_proxy
        .start(Default::default(), execution_controller_server_end)
        .await
        .unwrap()
        .unwrap();
    assert!(
        spawned_child.handles_receiver.next().await.is_some(),
        "failed to observe the local child be started"
    );
    assert!(spawned_child.component_status.is_running());
    execution_controller_proxy.stop().unwrap();
    if let Ok(Some(fcomponent::ExecutionControllerEvent::OnStop { stopped_payload })) =
        execution_controller_proxy.take_event_stream().try_next().await
    {
        assert_eq!(stopped_payload.status, Some(zx::Status::PEER_CLOSED.into_raw()));
    } else {
        panic!("expected OnStop to be called");
    }
    assert!(!spawned_child.component_status.is_running());
}

#[fuchsia::test]
pub async fn wait_for_exit() {
    let mut spawned_child = spawn_local_child().await;
    let (execution_controller_proxy, execution_controller_server_end) =
        create_proxy::<fcomponent::ExecutionControllerMarker>().unwrap();
    spawned_child
        .controller_proxy
        .start(Default::default(), execution_controller_server_end)
        .await
        .unwrap()
        .unwrap();
    assert!(
        spawned_child.handles_receiver.next().await.is_some(),
        "failed to observe the local child be started"
    );
    assert!(spawned_child.component_status.is_running());
    spawned_child.cancel_sender.take().unwrap().send(()).unwrap();
    if let Ok(Some(fcomponent::ExecutionControllerEvent::OnStop { stopped_payload })) =
        execution_controller_proxy.take_event_stream().try_next().await
    {
        assert_eq!(stopped_payload.status, Some(zx::Status::PEER_CLOSED.into_raw()));
    } else {
        panic!("expected OnStop to be called");
    }
    assert!(!spawned_child.component_status.is_running());
    let execution_controller_channel = execution_controller_proxy.into_channel().unwrap();
    fasync::OnSignals::new(&execution_controller_channel, zx::Signals::CHANNEL_PEER_CLOSED)
        .await
        .unwrap();
}

#[fuchsia::test]
pub async fn start_when_already_started() {
    let mut spawned_child = spawn_local_child().await;
    let (_execution_controller_proxy, execution_controller_server_end) =
        create_proxy::<fcomponent::ExecutionControllerMarker>().unwrap();
    spawned_child
        .controller_proxy
        .start(Default::default(), execution_controller_server_end)
        .await
        .unwrap()
        .unwrap();
    assert!(
        spawned_child.handles_receiver.next().await.is_some(),
        "failed to observe the local child be started"
    );
    assert!(spawned_child.component_status.is_running());
    let (execution_controller_proxy_2, execution_controller_server_end) =
        create_proxy::<fcomponent::ExecutionControllerMarker>().unwrap();
    let err = spawned_child
        .controller_proxy
        .start(Default::default(), execution_controller_server_end)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err, fcomponent::Error::InstanceAlreadyStarted);
    let execution_controller_channel = execution_controller_proxy_2.into_channel().unwrap();
    fasync::OnSignals::new(&execution_controller_channel, zx::Signals::CHANNEL_PEER_CLOSED)
        .await
        .unwrap();
}
