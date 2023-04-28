// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::Arc;

use anyhow::Error;
use appkit::*;
use either::Either;
use fidl::endpoints::{create_proxy, create_proxy_and_stream, DiscoverableProtocolMarker};
use fidl_fuchsia_element as felement;
use fidl_fuchsia_sysmem as sysmem;
use fidl_fuchsia_ui_composition as ui_comp;
use fidl_fuchsia_ui_focus as ui_focus;
use fidl_fuchsia_ui_input3 as ui_input3;
use fidl_fuchsia_ui_shortcut2 as ui_shortcut2;
use fidl_fuchsia_ui_test_scene as ui_test_scene;
use fuchsia_async as fasync;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use fuchsia_scenic::flatland::ViewCreationTokenPair;
use futures::{
    channel::mpsc::{unbounded, UnboundedReceiver},
    future, FutureExt, Stream, StreamExt,
};
use tracing::{error, info};

use crate::{
    shortcuts::ShortcutAction,
    wm::{
        get_first_view_creation_token, shell_view_name_annotation, Notification, ShellViewKind,
        WindowManager, WM_SHELLVIEW_APP_LAUNCHER,
    },
};

async fn set_up(
    sender: EventSender,
    receiver: &mut UnboundedReceiver<Event>,
) -> Result<(WindowManager, TestProtocolConnector, UnboundedReceiver<Notification>), Error> {
    let test_protocol_connector = TestProtocolConnector::new(build_realm().await?);

    attach_to_scene_manager(
        sender.clone(),
        test_protocol_connector.connect_to_test_scene_controller()?,
    );

    let (view_creation_token, view_spec_holders) = get_first_view_creation_token(receiver).await;

    let (notification_sender, notification_receiver) = unbounded::<Notification>();

    let wm = WindowManager::new(
        sender.clone(),
        notification_sender,
        view_creation_token,
        view_spec_holders,
        Box::new(test_protocol_connector.clone()),
    )?;
    Ok((wm, test_protocol_connector, notification_receiver))
}

fn loggable_event_receiver(receiver: UnboundedReceiver<Event>) -> impl Stream<Item = Event> {
    receiver.inspect(|event| info!("Parent event: {:?}", event))
}

#[fuchsia::test]
async fn test_wm() -> Result<(), Error> {
    let (sender, mut receiver) = EventSender::new();
    let (mut wm, test_protocol_connector, mut notification_receiver) =
        set_up(sender.clone(), &mut receiver).await?;
    let receiver = loggable_event_receiver(receiver);

    let loop_fut = wm.run(receiver);

    let cloned_test_protocol_connector = test_protocol_connector.clone();
    let test_fut = async move {
        let test_protocol_connector = cloned_test_protocol_connector;
        // Wait for window manager to get focus.
        wait_for_notification_callback(
            move |notification| match notification {
                Notification::FocusChanged { target: Either::Left(..) } => true,
                _ => false,
            },
            &mut notification_receiver,
        )
        .await;

        // Add child view 1. [ChildView1 (Focus)]
        info!("Add child_view1");
        let (child_view_id1, _, _child_task1) =
            create_child_view(sender.clone(), Box::new(test_protocol_connector.clone()));
        wait_for_notifications(
            &mut notification_receiver,
            vec![
                Notification::AddedChildView { id: child_view_id1 },
                Notification::FocusChanged { target: Either::Right(child_view_id1) },
            ],
        )
        .await;
        info!("Focused on child_view1: {:?}", child_view_id1);

        // Add child view 2. [ChildView1, ChildView2 (Focus)]
        info!("Add child_view2");
        let (child_view_id2, _, _child_task2) =
            create_child_view(sender.clone(), Box::new(test_protocol_connector.clone()));
        wait_for_notifications(
            &mut notification_receiver,
            vec![
                Notification::AddedChildView { id: child_view_id2 },
                Notification::FocusChanged { target: Either::Right(child_view_id2) },
            ],
        )
        .await;
        info!("Focused on child_view2: {:?}", child_view_id2);

        // Add child view 3. [ChildView1, ChildView2, ChildView3 (Focus)]
        info!("Add child_view3");
        let (child_view_id3, _, _child_task3) =
            create_child_view(sender.clone(), Box::new(test_protocol_connector.clone()));
        wait_for_notifications(
            &mut notification_receiver,
            vec![
                Notification::AddedChildView { id: child_view_id3 },
                Notification::FocusChanged { target: Either::Right(child_view_id3) },
            ],
        )
        .await;
        info!("Focused on child_view3: {:?}", child_view_id3);

        // Shortcut to focus on next view. Child view 1 should receive focus.
        // [ChildView2, ChildView3, ChildView1 (Focus)]
        info!("Switch focus next to  child_view1");
        let handled = invoke_shortcut(ShortcutAction::FocusNext, sender.clone()).await;
        assert!(matches!(handled, ui_shortcut2::Handled::Handled));
        wait_for_notification(
            &mut notification_receiver,
            Notification::FocusChanged { target: Either::Right(child_view_id1) },
        )
        .await;
        info!("Focused on child_view1: {:?}", child_view_id1);

        // Shortcut to focus on previous view. Child view 2 should receive focus.
        // [ChildView1, ChildView2, ChildView3 (Focus)]
        info!("Switch focus previous to  child_view2");
        let handled = invoke_shortcut(ShortcutAction::FocusPrev, sender.clone()).await;
        assert!(matches!(handled, ui_shortcut2::Handled::Handled));
        wait_for_notification(
            &mut notification_receiver,
            Notification::FocusChanged { target: Either::Right(child_view_id3) },
        )
        .await;
        info!("Focused on child_view3: {:?}", child_view_id3);

        // Shortcut to close active, top-most (ChildView3) view.  [ChildView1, ChildView2 (Focus)].
        info!("Close child_view3");
        let handled = invoke_shortcut(ShortcutAction::Close, sender.clone()).await;
        assert!(matches!(handled, ui_shortcut2::Handled::Handled));
        wait_for_notifications(
            &mut notification_receiver,
            vec![
                Notification::RemovedChildView { id: child_view_id3 },
                Notification::FocusChanged { target: Either::Right(child_view_id2) },
            ],
        )
        .await;
        info!("Focused on child_view2: {:?}", child_view_id2);

        sender.send(Event::Exit);

        futures::join!(_child_task1, _child_task2, _child_task3);
    }
    .boxed();

    let _ = future::join(loop_fut, test_fut).await;
    std::mem::drop(wm);
    test_protocol_connector.release().await?;
    Ok(())
}

#[fuchsia::test]
async fn test_no_windows() -> Result<(), Error> {
    let (sender, mut receiver) = EventSender::new();
    let (mut wm, test_protocol_connector, mut notification_receiver) =
        set_up(sender.clone(), &mut receiver).await?;
    let receiver = loggable_event_receiver(receiver);

    let loop_fut = wm.run(receiver);

    let test_fut = async move {
        wait_for_notification_callback(
            move |notification| match notification {
                Notification::FocusChanged { target: Either::Left(..) } => true,
                _ => false,
            },
            &mut notification_receiver,
        )
        .await;
        info!("Focused on self");

        sender.send(Event::Exit);
    }
    .boxed();

    let _ = future::join(loop_fut, test_fut).await;
    std::mem::drop(wm);
    test_protocol_connector.release().await?;
    Ok(())
}

#[fuchsia::test]
async fn test_dismiss_window_in_background() -> Result<(), Error> {
    let (sender, mut receiver) = EventSender::new();
    let (mut wm, test_protocol_connector, mut notification_receiver) =
        set_up(sender.clone(), &mut receiver).await?;
    let receiver = loggable_event_receiver(receiver);

    let loop_fut = wm.run(receiver);

    let cloned_test_protocol_connector = test_protocol_connector.clone();
    let test_fut = async move {
        let test_protocol_connector = cloned_test_protocol_connector;
        // Wait for window manager to get focus.
        wait_for_notification_callback(
            move |notification| match notification {
                Notification::FocusChanged { target: Either::Left(..) } => true,
                _ => false,
            },
            &mut notification_receiver,
        )
        .await;

        // Add child view 1. [ChildView1 (Focus)]
        info!("Add child_view1");
        let (child_view_id1, child_view_controller1, _child_task1) =
            create_child_view(sender.clone(), Box::new(test_protocol_connector.clone()));
        wait_for_notifications(
            &mut notification_receiver,
            vec![
                Notification::AddedChildView { id: child_view_id1 },
                Notification::FocusChanged { target: Either::Right(child_view_id1) },
            ],
        )
        .await;
        info!("Focused on child_view1: {:?}", child_view_id1);

        // Add child view 2. [ChildView1, ChildView2 (Focus)]
        info!("Add child_view2");
        let (child_view_id2, _, _child_task2) =
            create_child_view(sender.clone(), Box::new(test_protocol_connector.clone()));
        wait_for_notifications(
            &mut notification_receiver,
            vec![
                Notification::AddedChildView { id: child_view_id2 },
                Notification::FocusChanged { target: Either::Right(child_view_id2) },
            ],
        )
        .await;
        info!("Focused on child_view2: {:?}", child_view_id2);

        // Add child view 3. [ChildView1, ChildView2, ChildView3 (Focus)]
        info!("Add child_view3");
        let (child_view_id3, _, _child_task3) =
            create_child_view(sender.clone(), Box::new(test_protocol_connector.clone()));
        wait_for_notifications(
            &mut notification_receiver,
            vec![
                Notification::AddedChildView { id: child_view_id3 },
                Notification::FocusChanged { target: Either::Right(child_view_id3) },
            ],
        )
        .await;
        info!("Focused on child_view3: {:?}", child_view_id3);

        // Dismiss the background ChildView1. [ChildView2, ChildView3 (Focus)].
        info!("Dismissing ChildView1");
        child_view_controller1.dismiss().expect("Failed to dismiss ChildView1");
        wait_for_notification(
            &mut notification_receiver,
            Notification::RemovedChildView { id: child_view_id1 },
        )
        .await;

        // Nothing should crash.
        sender.send(Event::Exit);

        futures::join!(_child_task1, _child_task2, _child_task3);
    }
    .boxed();

    let _ = future::join(loop_fut, test_fut).await;
    std::mem::drop(wm);
    test_protocol_connector.release().await?;
    Ok(())
}

#[fuchsia::test]
async fn test_add_shell_view() -> Result<(), Error> {
    let (sender, mut receiver) = EventSender::new();
    let (mut wm, test_protocol_connector, mut notification_receiver) =
        set_up(sender.clone(), &mut receiver).await?;
    let receiver = loggable_event_receiver(receiver);

    let loop_fut = wm.run(receiver);

    let cloned_test_protocol_connector = test_protocol_connector.clone();
    let test_fut = async move {
        let test_protocol_connector = cloned_test_protocol_connector;

        let name_annotation = shell_view_name_annotation(WM_SHELLVIEW_APP_LAUNCHER);
        let (shell_view_id, _, shell_view_task) = add_child_view(
            sender.clone(),
            Box::new(test_protocol_connector.clone()),
            Some(name_annotation),
        );
        wait_for_notification(
            &mut notification_receiver,
            Notification::AddedShellView { id: shell_view_id, kind: ShellViewKind::AppLauncher },
        )
        .await;
        info!("Added shell view: {:?}", WM_SHELLVIEW_APP_LAUNCHER);

        sender.send(Event::Exit);

        futures::join!(shell_view_task);
    }
    .boxed();

    let _ = future::join(loop_fut, test_fut).await;
    std::mem::drop(wm);
    test_protocol_connector.release().await?;
    Ok(())
}

#[fuchsia::test]
async fn test_immediately_close() -> Result<(), Error> {
    let (sender, mut receiver) = EventSender::new();
    let (mut wm, test_protocol_connector, _) = set_up(sender.clone(), &mut receiver).await?;
    let receiver = loggable_event_receiver(receiver);

    let loop_fut = wm.run(receiver);

    let cloned_test_protocol_connector = test_protocol_connector.clone();
    let test_fut = async move {
        let test_protocol_connector = cloned_test_protocol_connector;
        info!("Add child_view1");
        let (_, _, child_view_task) =
            create_child_view(sender.clone(), Box::new(test_protocol_connector.clone()));

        sender.send(Event::Exit);

        futures::join!(child_view_task);
    }
    .boxed();

    let _ = future::join(loop_fut, test_fut).await;
    std::mem::drop(wm);
    test_protocol_connector.release().await?;
    Ok(())
}

#[fuchsia::test]
async fn test_dismiss_before_attach() -> Result<(), Error> {
    let (sender, mut receiver) = EventSender::new();
    let (mut wm, test_protocol_connector, _) = set_up(sender.clone(), &mut receiver).await?;
    let receiver = loggable_event_receiver(receiver);

    let loop_fut = wm.run(receiver);

    let cloned_test_protocol_connector = test_protocol_connector.clone();
    let test_fut = async move {
        let test_protocol_connector = cloned_test_protocol_connector;
        info!("Add child_view1");
        let (_, child_view_controller, child_view_task) =
            create_child_view(sender.clone(), Box::new(test_protocol_connector.clone()));
        child_view_controller.dismiss().expect("Failed to dismiss childview1");

        sender.send(Event::Exit);

        futures::join!(child_view_task);
    }
    .boxed();

    let _ = future::join(loop_fut, test_fut).await;
    std::mem::drop(wm);
    test_protocol_connector.release().await?;
    Ok(())
}

fn attach_to_scene_manager(
    event_sender: EventSender,
    scene_provider: ui_test_scene::ControllerProxy,
) {
    let ViewCreationTokenPair { view_creation_token, viewport_creation_token } =
        ViewCreationTokenPair::new().expect("Failed to create ViewCreationTokenPair");

    event_sender.send(Event::SystemEvent {
        event: SystemEvent::ViewCreationToken { token: view_creation_token },
    });

    if let Err(e) =
        scene_provider.present_client_view(ui_test_scene::ControllerPresentClientViewRequest {
            viewport_creation_token: Some(viewport_creation_token),
            ..Default::default()
        })
    {
        error!("Failed to present viewport_creation_token to test SceneProvider: {:?}", e);
    }
}

fn create_child_view(
    parent_sender: EventSender,
    protocol_connector: Box<dyn ProtocolConnector>,
) -> (ChildViewId, felement::ViewControllerProxy, fasync::Task<()>) {
    add_child_view(parent_sender, protocol_connector, None)
}

fn add_child_view(
    parent_sender: EventSender,
    protocol_connector: Box<dyn ProtocolConnector>,
    annotation: Option<felement::Annotation>,
) -> (ChildViewId, felement::ViewControllerProxy, fasync::Task<()>) {
    let ViewCreationTokenPair { view_creation_token, viewport_creation_token } =
        ViewCreationTokenPair::new().expect("Fidl error");
    let child_view_id = ChildViewId::from_viewport_creation_token(&viewport_creation_token);

    let (view_controller_proxy, view_controller_request) =
        create_proxy::<felement::ViewControllerMarker>().expect("Fidl error");
    let view_spec = felement::ViewSpec {
        viewport_creation_token: Some(viewport_creation_token),
        annotations: annotation.map(|a| vec![a]),
        ..Default::default()
    };
    parent_sender.send(Event::SystemEvent {
        event: SystemEvent::PresentViewSpec {
            view_spec_holder: ViewSpecHolder {
                view_spec,
                annotation_controller: None,
                view_controller_request: Some(view_controller_request),
                responder: None,
            },
        },
    });

    let task = fasync::Task::spawn(async move {
        let (sender, mut receiver) = EventSender::new();

        let mut window = Window::new(sender)
            .with_view_creation_token(view_creation_token)
            .with_protocol_connector(protocol_connector);
        window.create_view().expect("Failed to create window for child view");

        while let Some(child_event) = receiver.next().await {
            info!("Child event: {:?}", child_event);
            if matches!(child_event, Event::WindowEvent { event: WindowEvent::Closed, .. }) {
                break;
            }
        }
    });

    (child_view_id, view_controller_proxy, task)
}

async fn wait_for_notifications(
    receiver: &mut UnboundedReceiver<Notification>,
    mut notifications: Vec<Notification>,
) {
    for notification in notifications.drain(..) {
        wait_for_notification(receiver, notification).await;
    }
}

async fn wait_for_notification(
    receiver: &mut UnboundedReceiver<Notification>,
    notification: Notification,
) {
    wait_for_notification_callback(move |notif| &notification == notif, receiver).await;
}

async fn wait_for_notification_callback(
    mut callback: impl FnMut(&Notification) -> bool,
    receiver: &mut UnboundedReceiver<Notification>,
) {
    while let Some(notification) = receiver.next().await {
        info!("Notification: {:?}", notification);
        if callback(&notification) {
            return;
        }
    }
    unreachable!()
}

async fn invoke_shortcut(action: ShortcutAction, sender: EventSender) -> ui_shortcut2::Handled {
    let (listener_request, mut listener_stream) =
        create_proxy_and_stream::<ui_shortcut2::ListenerMarker>()
            .expect("Failed to create proxy and stream");

    fasync::Task::local(async move {
        if let Some(request) = listener_stream.next().await {
            match request {
                Ok(ui_shortcut2::ListenerRequest::OnShortcut { id, responder }) => {
                    sender.send(Event::WindowEvent {
                        window_id: WindowId(0),
                        event: WindowEvent::Shortcut { id, responder },
                    });
                }
                Err(fidl::Error::ClientChannelClosed { .. }) => {
                    error!("Shortcut listener connection closed.");
                }
                Err(fidl_error) => {
                    error!("Shortcut listener error: {:?}", fidl_error);
                }
            }
        }
    })
    .detach();

    listener_request.on_shortcut(action as u32).await.expect("Failed to call on_shortcut")
}

#[derive(Clone)]
pub struct TestProtocolConnector(Arc<RealmInstance>);

impl ProtocolConnector for TestProtocolConnector {
    fn connect_to_flatland(&self) -> Result<ui_comp::FlatlandProxy, Error> {
        self.connect_to_protocol::<ui_comp::FlatlandMarker>()
    }

    fn connect_to_graphical_presenter(&self) -> Result<felement::GraphicalPresenterProxy, Error> {
        self.connect_to_protocol::<felement::GraphicalPresenterMarker>()
    }

    fn connect_to_shortcuts_registry(&self) -> Result<ui_shortcut2::RegistryProxy, Error> {
        self.connect_to_protocol::<ui_shortcut2::RegistryMarker>()
    }

    fn connect_to_keyboard(&self) -> Result<ui_input3::KeyboardProxy, Error> {
        self.connect_to_protocol::<ui_input3::KeyboardMarker>()
    }

    fn connect_to_focus_chain_listener(
        &self,
    ) -> Result<ui_focus::FocusChainListenerRegistryProxy, Error> {
        self.connect_to_protocol::<ui_focus::FocusChainListenerRegistryMarker>()
    }

    fn connect_to_sysmem_allocator(&self) -> Result<sysmem::AllocatorProxy, Error> {
        connect_to_protocol::<sysmem::AllocatorMarker>()
    }

    fn connect_to_flatland_allocator(&self) -> Result<ui_comp::AllocatorProxy, Error> {
        self.connect_to_protocol::<ui_comp::AllocatorMarker>()
    }

    fn box_clone(&self) -> Box<dyn ProtocolConnector> {
        Box::new(TestProtocolConnector(self.0.clone()))
    }
}

impl TestProtocolConnector {
    fn new(realm: RealmInstance) -> Self {
        Self(Arc::new(realm))
    }

    async fn release(self) -> Result<(), Error> {
        let ref_count = Arc::strong_count(&self.0);
        let realm = if let Ok(realm) = Arc::try_unwrap(self.0) {
            realm
        } else {
            panic!("Failed to release test realm instance. {:?} references still exist", ref_count)
        };
        realm.destroy().await?;
        Ok(())
    }

    fn connect_to_protocol<P: DiscoverableProtocolMarker>(&self) -> Result<P::Proxy, Error>
    where
        P: DiscoverableProtocolMarker,
    {
        self.0.root.connect_to_protocol_at_exposed_dir::<P>()
    }

    fn connect_to_test_scene_controller(&self) -> Result<ui_test_scene::ControllerProxy, Error> {
        self.connect_to_protocol::<ui_test_scene::ControllerMarker>()
    }
}

async fn build_realm() -> anyhow::Result<RealmInstance> {
    let builder = RealmBuilder::new().await?;

    let test_ui_stack =
        builder.add_child("test-ui-stack", "#meta/test-ui-stack.cm", ChildOptions::new()).await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.ui.composition.Allocator"))
                .capability(Capability::protocol_by_name("fuchsia.ui.composition.Flatland"))
                .capability(Capability::protocol_by_name("fuchsia.ui.composition.Screenshot"))
                .capability(Capability::protocol_by_name(
                    "fuchsia.ui.focus.FocusChainListenerRegistry",
                ))
                .capability(Capability::protocol_by_name("fuchsia.ui.input3.Keyboard"))
                .capability(Capability::protocol_by_name("fuchsia.ui.shortcut2.Registry"))
                .capability(Capability::protocol_by_name("fuchsia.ui.test.input.Registry"))
                .capability(Capability::protocol_by_name("fuchsia.ui.test.scene.Controller"))
                .from(&test_ui_stack)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.logger.LogSink"))
                .capability(Capability::protocol_by_name("fuchsia.scheduler.ProfileProvider"))
                .capability(Capability::protocol_by_name("fuchsia.sysmem.Allocator"))
                .capability(Capability::protocol_by_name("fuchsia.tracing.provider.Registry"))
                .capability(Capability::protocol_by_name("fuchsia.vulkan.loader.Loader"))
                .from(Ref::parent())
                .to(&test_ui_stack),
        )
        .await?;

    let realm = builder.build().await?;
    Ok(realm)
}
