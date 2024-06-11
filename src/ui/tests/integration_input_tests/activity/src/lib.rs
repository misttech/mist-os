// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_utils::hanging_get::client::HangingGetStream;
use fidl::endpoints::create_proxy;
use fidl_fuchsia_input_interaction::{NotifierMarker, NotifierProxy, State};
use fidl_fuchsia_input_report::{ConsumerControlButton, KeyboardInputReport};
use fidl_fuchsia_logger::LogSinkMarker;
use fidl_fuchsia_math::Vec_;
use fidl_fuchsia_scheduler::RoleManagerMarker;
use fidl_fuchsia_tracing_provider::RegistryMarker;
use fidl_fuchsia_ui_test_input::{
    KeyboardMarker, KeyboardSimulateKeyEventRequest, MediaButtonsDeviceMarker,
    MediaButtonsDeviceSimulateButtonPressRequest, MouseMarker, MouseSimulateMouseEventRequest,
    RegistryMarker as InputRegistryMarker, RegistryRegisterKeyboardRequest,
    RegistryRegisterMediaButtonsDeviceRequest, RegistryRegisterMouseRequest,
    RegistryRegisterTouchScreenRequest, TouchScreenMarker, TouchScreenSimulateTapRequest,
};
use fidl_fuchsia_vulkan_loader::LoaderMarker;
use fuchsia_async::{Time, Timer};
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, RealmInstance, Ref, Route};
use fuchsia_zircon::Duration;
use futures::{future, StreamExt};

const TEST_UI_STACK: &str = "ui";
const TEST_UI_STACK_URL: &str = "flatland-scene-manager-test-ui-stack#meta/test-ui-stack.cm";

// Set a maximum bound test timeout.
const TEST_TIMEOUT: Duration = Duration::from_minutes(2);

async fn assemble_realm() -> RealmInstance {
    let builder = RealmBuilder::new().await.expect("Failed to create RealmBuilder.");

    // Add test UI stack component.
    builder
        .add_child(TEST_UI_STACK, TEST_UI_STACK_URL, ChildOptions::new())
        .await
        .expect("Failed to add UI realm.");

    // Route capabilities to the test UI realm.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LogSinkMarker>())
                .capability(Capability::protocol::<fidl_fuchsia_sysmem::AllocatorMarker>())
                .capability(Capability::protocol::<fidl_fuchsia_sysmem2::AllocatorMarker>())
                .capability(Capability::protocol::<LoaderMarker>())
                .capability(Capability::protocol::<RegistryMarker>())
                .capability(Capability::protocol::<RoleManagerMarker>())
                .from(Ref::parent())
                .to(Ref::child(TEST_UI_STACK)),
        )
        .await
        .expect("Failed to route capabilities.");

    // Route capabilities from the test UI realm.
    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<NotifierMarker>())
                .capability(Capability::protocol::<InputRegistryMarker>())
                .from(Ref::child(TEST_UI_STACK))
                .to(Ref::parent()),
        )
        .await
        .expect("Failed to route capabilities.");

    // Create the test realm.
    builder.build().await.expect("Failed to create test realm.")
}

#[fuchsia::test]
async fn enters_idle_state_without_activity() {
    let realm = assemble_realm().await;

    // Subscribe to activity state, which serves "Active" initially.
    let notifier_proxy = realm
        .root
        .connect_to_protocol_at_exposed_dir::<NotifierMarker>()
        .expect("Failed to connect to fuchsia.input.interaction.Notifier.");
    let mut watch_state_stream = HangingGetStream::new(notifier_proxy, NotifierProxy::watch_state);
    assert_eq!(
        watch_state_stream
            .next()
            .await
            .expect("Expected initial state from watch_state()")
            .unwrap(),
        State::Active
    );

    // Do nothing. Activity service transitions to idle state in one minute.
    let activity_timeout_upper_bound = Timer::new(Time::after(TEST_TIMEOUT));
    match future::select(watch_state_stream.next(), activity_timeout_upper_bound).await {
        future::Either::Left((result, _)) => {
            assert_eq!(result.unwrap().expect("Expected state transition."), State::Idle);
        }
        future::Either::Right(_) => panic!("Timer expired before state transitioned."),
    }

    // Shut down input pipeline before dropping mocks, so that input pipeline
    // doesn't log errors about channels being closed.
    realm.destroy().await.expect("Failed to shut down realm.");
}

#[fuchsia::test]
async fn does_not_enter_active_state_with_keyboard() {
    let realm = assemble_realm().await;

    // Subscribe to activity state, which serves "Active" initially.
    let notifier_proxy = realm
        .root
        .connect_to_protocol_at_exposed_dir::<NotifierMarker>()
        .expect("Failed to connect to fuchsia.input.interaction.Notifier.");
    let mut watch_state_stream = HangingGetStream::new(notifier_proxy, NotifierProxy::watch_state);
    assert_eq!(
        watch_state_stream
            .next()
            .await
            .expect("Expected initial state from watch_state()")
            .unwrap(),
        State::Active
    );

    // Do nothing. Activity service transitions to idle state in one minute.
    let activity_timeout_upper_bound = Timer::new(Time::after(TEST_TIMEOUT));
    match future::select(watch_state_stream.next(), activity_timeout_upper_bound).await {
        future::Either::Left((result, _)) => {
            assert_eq!(result.unwrap().expect("Expected state transition."), State::Idle);
        }
        future::Either::Right(_) => panic!("Timer expired before state transitioned."),
    }

    // Inject keyboard input.
    let (keyboard_proxy, keyboard_server) =
        create_proxy::<KeyboardMarker>().expect("Failed to create KeyboardProxy.");
    let input_registry = realm
        .root
        .connect_to_protocol_at_exposed_dir::<InputRegistryMarker>()
        .expect("Failed to connect to fuchsia.ui.test.input.Registry.");
    input_registry
        .register_keyboard(RegistryRegisterKeyboardRequest {
            device: Some(keyboard_server),
            ..Default::default()
        })
        .await
        .expect("Failed to register keyboard device.");
    keyboard_proxy
        .simulate_key_event(&KeyboardSimulateKeyEventRequest {
            report: Some(KeyboardInputReport {
                pressed_keys3: Some(vec![fidl_fuchsia_input::Key::A]),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .expect("Failed to send key event 'a'.");

    // Activity service does not transition to active state.
    let activity_timeout_upper_bound = Timer::new(Time::after(TEST_TIMEOUT));
    match future::select(watch_state_stream.next(), activity_timeout_upper_bound).await {
        future::Either::Left((_, _)) => {
            panic!("Activity should not have changed.");
        }
        future::Either::Right(_) => {}
    }

    // Shut down input pipeline before dropping mocks, so that input pipeline
    // doesn't log errors about channels being closed.
    realm.destroy().await.expect("Failed to shut down realm.");
}

#[fuchsia::test]
async fn enters_active_state_with_mouse() {
    let realm = assemble_realm().await;

    // Subscribe to activity state, which serves "Active" initially.
    let notifier_proxy = realm
        .root
        .connect_to_protocol_at_exposed_dir::<NotifierMarker>()
        .expect("Failed to connect to fuchsia.input.interaction.Notifier.");
    let mut watch_state_stream = HangingGetStream::new(notifier_proxy, NotifierProxy::watch_state);
    assert_eq!(
        watch_state_stream
            .next()
            .await
            .expect("Expected initial state from watch_state()")
            .unwrap(),
        State::Active
    );

    // Do nothing. Activity service transitions to idle state in one minute.
    let activity_timeout_upper_bound = Timer::new(Time::after(TEST_TIMEOUT));
    match future::select(watch_state_stream.next(), activity_timeout_upper_bound).await {
        future::Either::Left((result, _)) => {
            assert_eq!(result.unwrap().expect("Expected state transition."), State::Idle);
        }
        future::Either::Right(_) => panic!("Timer expired before state transitioned."),
    }

    // Inject mouse input.
    let (mouse_proxy, mouse_server) =
        create_proxy::<MouseMarker>().expect("Failed to create MouseProxy.");
    let input_registry = realm
        .root
        .connect_to_protocol_at_exposed_dir::<InputRegistryMarker>()
        .expect("Failed to connect to fuchsia.ui.test.input.Registry.");
    input_registry
        .register_mouse(RegistryRegisterMouseRequest {
            device: Some(mouse_server),
            ..Default::default()
        })
        .await
        .expect("Failed to register mouse device.");
    mouse_proxy
        .simulate_mouse_event(&MouseSimulateMouseEventRequest {
            movement_x: Some(10),
            movement_y: Some(15),
            ..Default::default()
        })
        .await
        .expect("Failed to send mouse movement to location (10, 15).");

    // Activity service transitions to active state.
    assert_eq!(
        watch_state_stream
            .next()
            .await
            .expect("Expected updated state from watch_state()")
            .unwrap(),
        State::Active
    );

    // Shut down input pipeline before dropping mocks, so that input pipeline
    // doesn't log errors about channels being closed.
    realm.destroy().await.expect("Failed to shut down realm.");
}

#[fuchsia::test]
async fn enters_active_state_with_touchscreen() {
    let realm = assemble_realm().await;

    // Subscribe to activity state, which serves "Active" initially.
    let notifier_proxy = realm
        .root
        .connect_to_protocol_at_exposed_dir::<NotifierMarker>()
        .expect("Failed to connect to fuchsia.input.interaction.Notifier.");
    let mut watch_state_stream = HangingGetStream::new(notifier_proxy, NotifierProxy::watch_state);
    assert_eq!(
        watch_state_stream
            .next()
            .await
            .expect("Expected initial state from watch_state()")
            .unwrap(),
        State::Active
    );

    // Do nothing. Activity service transitions to idle state in one minute.
    let activity_timeout_upper_bound = Timer::new(Time::after(TEST_TIMEOUT));
    match future::select(watch_state_stream.next(), activity_timeout_upper_bound).await {
        future::Either::Left((result, _)) => {
            assert_eq!(result.unwrap().expect("Expected state transition."), State::Idle);
        }
        future::Either::Right(_) => panic!("Timer expired before state transitioned."),
    }

    // Inject touch input.
    let (touchscreen_proxy, touchscreen_server) =
        create_proxy::<TouchScreenMarker>().expect("Failed to create TouchScreenProxy.");
    let input_registry = realm
        .root
        .connect_to_protocol_at_exposed_dir::<InputRegistryMarker>()
        .expect("Failed to connect to fuchsia.ui.test.input.Registry.");
    input_registry
        .register_touch_screen(RegistryRegisterTouchScreenRequest {
            device: Some(touchscreen_server),
            ..Default::default()
        })
        .await
        .expect("Failed to register touchscreen device.");
    touchscreen_proxy
        .simulate_tap(&TouchScreenSimulateTapRequest {
            tap_location: Some(Vec_ { x: 0, y: 0 }),
            ..Default::default()
        })
        .await
        .expect("Failed to simulate tap at location (0, 0).");

    // Activity service transitions to active state.
    assert_eq!(
        watch_state_stream
            .next()
            .await
            .expect("Expected updated state from watch_state()")
            .unwrap(),
        State::Active
    );

    // Shut down input pipeline before dropping mocks, so that input pipeline
    // doesn't log errors about channels being closed.
    realm.destroy().await.expect("Failed to shut down realm.");
}

#[fuchsia::test]
async fn enters_active_state_with_media_buttons() {
    let realm = assemble_realm().await;

    // Subscribe to activity state, which serves "Active" initially.
    let notifier_proxy = realm
        .root
        .connect_to_protocol_at_exposed_dir::<NotifierMarker>()
        .expect("Failed to connect to fuchsia.input.interaction.Notifier.");
    let mut watch_state_stream = HangingGetStream::new(notifier_proxy, NotifierProxy::watch_state);
    assert_eq!(
        watch_state_stream
            .next()
            .await
            .expect("Expected initial state from watch_state()")
            .unwrap(),
        State::Active
    );

    // Do nothing. Activity service transitions to idle state in one minute.
    let activity_timeout_upper_bound = Timer::new(Time::after(TEST_TIMEOUT));
    match future::select(watch_state_stream.next(), activity_timeout_upper_bound).await {
        future::Either::Left((result, _)) => {
            assert_eq!(result.unwrap().expect("Expected state transition."), State::Idle);
        }
        future::Either::Right(_) => panic!("Timer expired before state transitioned."),
    }

    // Inject media buttons input.
    let (media_buttons_proxy, media_buttons_server) =
        create_proxy::<MediaButtonsDeviceMarker>().expect("Failed to create MediaButtonsProxy");
    let input_registry = realm
        .root
        .connect_to_protocol_at_exposed_dir::<InputRegistryMarker>()
        .expect("Failed to connect to fuchsia.ui.test.input.Registry.");
    input_registry
        .register_media_buttons_device(RegistryRegisterMediaButtonsDeviceRequest {
            device: Some(media_buttons_server),
            ..Default::default()
        })
        .await
        .expect("Failed to register media buttons device.");
    media_buttons_proxy
        .simulate_button_press(&MediaButtonsDeviceSimulateButtonPressRequest {
            button: Some(ConsumerControlButton::VolumeUp),
            ..Default::default()
        })
        .await
        .expect("Failed to simulate buttons press.");

    // Activity service transitions to active state.
    assert_eq!(
        watch_state_stream
            .next()
            .await
            .expect("Expected updated state from watch_state()")
            .unwrap(),
        State::Active
    );

    // Shut down input pipeline before dropping mocks, so that input pipeline
    // doesn't log errors about channels being closed.
    realm.destroy().await.expect("Failed to shut down realm.");
}
