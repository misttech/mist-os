// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(clippy::await_holding_refcell_ref)]
use crate::input_handler::{InputHandlerStatus, UnhandledInputHandler};
use crate::utils::{Position, Size};
use crate::{input_device, metrics, touch_binding};
use anyhow::{Context, Error, Result};
use async_trait::async_trait;
use async_utils::hanging_get::client::HangingGetStream;
use fidl::endpoints::{create_proxy, Proxy};
use fidl::AsHandleRef;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_inspect::health::Reporter;
use futures::channel::mpsc;
use futures::stream::StreamExt;
use metrics_registry::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use {
    fidl_fuchsia_input_report as fidl_input_report, fidl_fuchsia_ui_input as fidl_ui_input,
    fidl_fuchsia_ui_pointerinjector as pointerinjector,
    fidl_fuchsia_ui_pointerinjector_configuration as pointerinjector_config,
    fidl_fuchsia_ui_policy as fidl_ui_policy, fuchsia_async as fasync,
};

/// An input handler that parses touch events and forwards them to Scenic through the
/// fidl_fuchsia_pointerinjector protocols.
pub struct TouchInjectorHandler {
    /// The mutable fields of this handler.
    mutable_state: RefCell<MutableState>,

    /// The scope and coordinate system of injection.
    /// See fidl_fuchsia_pointerinjector::Context for more details.
    context_view_ref: fidl_fuchsia_ui_views::ViewRef,

    /// The region where dispatch is attempted for injected events.
    /// See fidl_fuchsia_pointerinjector::Target for more details.
    target_view_ref: fidl_fuchsia_ui_views::ViewRef,

    /// The size of the display associated with the touch device, used to convert
    /// coordinates from the touch input report to device coordinates (which is what
    /// Scenic expects).
    display_size: Size,

    /// The FIDL proxy to register new injectors.
    injector_registry_proxy: pointerinjector::RegistryProxy,

    /// The FIDL proxy used to get configuration details for pointer injection.
    configuration_proxy: pointerinjector_config::SetupProxy,

    /// The inventory of this handler's Inspect status.
    pub inspect_status: InputHandlerStatus,

    /// The metrics logger.
    metrics_logger: metrics::MetricsLogger,
}

#[derive(Debug)]
struct MutableState {
    /// A rectangular region that directs injected events into a target.
    /// See fidl_fuchsia_pointerinjector::Viewport for more details.
    viewport: Option<pointerinjector::Viewport>,

    /// The injectors registered with Scenic, indexed by their device ids.
    injectors: HashMap<u32, pointerinjector::DeviceProxy>,

    /// The touch button listeners, key referenced by proxy channel's raw handle.
    pub listeners: HashMap<u32, fidl_ui_policy::TouchButtonsListenerProxy>,

    /// The last TouchButtonsEvent sent to all listeners.
    /// This is used to send new listeners the state of the touchscreen buttons.
    pub last_button_event: Option<fidl_ui_input::TouchButtonsEvent>,

    pub send_event_task_tracker: LocalTaskTracker,
}

#[async_trait(?Send)]
impl UnhandledInputHandler for TouchInjectorHandler {
    async fn handle_unhandled_input_event(
        self: Rc<Self>,
        unhandled_input_event: input_device::UnhandledInputEvent,
    ) -> Vec<input_device::InputEvent> {
        fuchsia_trace::duration!(c"input", c"presentation_on_event");
        match unhandled_input_event {
            input_device::UnhandledInputEvent {
                device_event: input_device::InputDeviceEvent::TouchScreen(ref touch_event),
                device_descriptor:
                    input_device::InputDeviceDescriptor::TouchScreen(ref touch_device_descriptor),
                event_time,
                trace_id,
            } => {
                self.inspect_status.count_received_event(input_device::InputEvent::from(
                    unhandled_input_event.clone(),
                ));
                fuchsia_trace::duration!(c"input", c"touch_injector_handler");
                if let Some(trace_id) = trace_id {
                    fuchsia_trace::flow_end!(c"input", c"event_in_input_pipeline", trace_id.into());
                }
                if touch_event.injector_contacts.values().all(|vec| vec.is_empty()) {
                    let touch_buttons_event = Self::create_touch_buttons_event(
                        &touch_event,
                        event_time,
                        &touch_device_descriptor,
                    );

                    // Send the event if the touch buttons are supported.
                    self.send_event_to_listeners(&touch_buttons_event).await;

                    // Store the sent event.
                    self.mutable_state.borrow_mut().last_button_event = Some(touch_buttons_event);
                } else if touch_event.pressed_buttons.is_empty() {
                    // Create a new injector if this is the first time seeing device_id.
                    if let Err(e) = self.ensure_injector_registered(&touch_device_descriptor).await
                    {
                        self.metrics_logger.log_error(
                        InputPipelineErrorMetricDimensionEvent::TouchInjectorEnsureInjectorRegisteredFailed,
                        std::format!("ensure_injector_registered failed: {}", e));
                    }

                    // Handle the event.
                    if let Err(e) = self
                        .send_event_to_scenic(&touch_event, &touch_device_descriptor, event_time)
                        .await
                    {
                        self.metrics_logger.log_error(
                        InputPipelineErrorMetricDimensionEvent::TouchInjectorSendEventToScenicFailed,
                        std::format!("send_event_to_scenic failed: {}", e));
                    }
                }

                // Consume the input event.
                self.inspect_status.count_handled_event();
                vec![input_device::InputEvent::from(unhandled_input_event).into_handled()]
            }
            _ => vec![input_device::InputEvent::from(unhandled_input_event)],
        }
    }

    fn set_handler_healthy(self: std::rc::Rc<Self>) {
        self.inspect_status.health_node.borrow_mut().set_ok();
    }

    fn set_handler_unhealthy(self: std::rc::Rc<Self>, msg: &str) {
        self.inspect_status.health_node.borrow_mut().set_unhealthy(msg);
    }
}

impl TouchInjectorHandler {
    /// Creates a new touch handler that holds touch pointer injectors.
    /// The caller is expected to spawn a task to continually watch for updates to the viewport.
    /// Example:
    /// let handler = TouchInjectorHandler::new(display_size).await?;
    /// fasync::Task::local(handler.clone().watch_viewport()).detach();
    ///
    /// # Parameters
    /// - `display_size`: The size of the associated touch display.
    ///
    /// # Errors
    /// If unable to connect to pointerinjector protocols.
    pub async fn new(
        display_size: Size,
        input_handlers_node: &fuchsia_inspect::Node,
        metrics_logger: metrics::MetricsLogger,
    ) -> Result<Rc<Self>, Error> {
        let configuration_proxy = connect_to_protocol::<pointerinjector_config::SetupMarker>()?;
        let injector_registry_proxy = connect_to_protocol::<pointerinjector::RegistryMarker>()?;

        Self::new_handler(
            configuration_proxy,
            injector_registry_proxy,
            display_size,
            input_handlers_node,
            metrics_logger,
        )
        .await
    }

    /// Creates a new touch handler that holds touch pointer injectors.
    /// The caller is expected to spawn a task to continually watch for updates to the viewport.
    /// Example:
    /// let handler = TouchInjectorHandler::new_with_config_proxy(config_proxy, display_size).await?;
    /// fasync::Task::local(handler.clone().watch_viewport()).detach();
    ///
    /// # Parameters
    /// - `configuration_proxy`: A proxy used to get configuration details for pointer
    ///    injection.
    /// - `display_size`: The size of the associated touch display.
    ///
    /// # Errors
    /// If unable to get injection view refs from `configuration_proxy`.
    /// If unable to connect to pointerinjector Registry protocol.
    pub async fn new_with_config_proxy(
        configuration_proxy: pointerinjector_config::SetupProxy,
        display_size: Size,
        input_handlers_node: &fuchsia_inspect::Node,
        metrics_logger: metrics::MetricsLogger,
    ) -> Result<Rc<Self>, Error> {
        let injector_registry_proxy = connect_to_protocol::<pointerinjector::RegistryMarker>()?;
        Self::new_handler(
            configuration_proxy,
            injector_registry_proxy,
            display_size,
            input_handlers_node,
            metrics_logger,
        )
        .await
    }

    /// Creates a new touch handler that holds touch pointer injectors.
    /// The caller is expected to spawn a task to continually watch for updates to the viewport.
    /// Example:
    /// let handler = TouchInjectorHandler::new_handler(None, None, display_size).await?;
    /// fasync::Task::local(handler.clone().watch_viewport()).detach();
    ///
    /// # Parameters
    /// - `configuration_proxy`: A proxy used to get configuration details for pointer
    ///    injection.
    /// - `injector_registry_proxy`: A proxy used to register new pointer injectors.  If
    ///    none is provided, connect to protocol routed to this component.
    /// - `display_size`: The size of the associated touch display.
    ///
    /// # Errors
    /// If unable to get injection view refs from `configuration_proxy`.
    async fn new_handler(
        configuration_proxy: pointerinjector_config::SetupProxy,
        injector_registry_proxy: pointerinjector::RegistryProxy,
        display_size: Size,
        input_handlers_node: &fuchsia_inspect::Node,
        metrics_logger: metrics::MetricsLogger,
    ) -> Result<Rc<Self>, Error> {
        // Get the context and target views to inject into.
        let (context_view_ref, target_view_ref) = configuration_proxy.get_view_refs().await?;

        let inspect_status = InputHandlerStatus::new(
            input_handlers_node,
            "touch_injector_handler",
            /* generates_events */ false,
        );
        let handler = Rc::new(Self {
            mutable_state: RefCell::new(MutableState {
                viewport: None,
                injectors: HashMap::new(),
                listeners: HashMap::new(),
                last_button_event: None,
                send_event_task_tracker: LocalTaskTracker::new(),
            }),
            context_view_ref,
            target_view_ref,
            display_size,
            injector_registry_proxy,
            configuration_proxy,
            inspect_status,
            metrics_logger,
        });

        Ok(handler)
    }

    /// Adds a new pointer injector and tracks it in `self.injectors` if one doesn't exist at
    /// `touch_descriptor.device_id`.
    ///
    /// # Parameters
    /// - `touch_descriptor`: The descriptor of the new touch device.
    async fn ensure_injector_registered(
        self: &Rc<Self>,
        touch_descriptor: &touch_binding::TouchScreenDeviceDescriptor,
    ) -> Result<(), anyhow::Error> {
        if self.mutable_state.borrow().injectors.contains_key(&touch_descriptor.device_id) {
            return Ok(());
        }

        // Create a new injector.
        let (device_proxy, device_server) = create_proxy::<pointerinjector::DeviceMarker>();
        let context = fuchsia_scenic::duplicate_view_ref(&self.context_view_ref)
            .context("Failed to duplicate context view ref.")?;
        let target = fuchsia_scenic::duplicate_view_ref(&self.target_view_ref)
            .context("Failed to duplicate target view ref.")?;
        let viewport = self.mutable_state.borrow().viewport.clone();
        if viewport.is_none() {
            // An injector without a viewport is not valid. The event will be dropped
            // since the handler will not have a registered injector to inject into.
            return Err(anyhow::format_err!(
                "Received a touch event without a viewport to inject into."
            ));
        }
        let config = pointerinjector::Config {
            device_id: Some(touch_descriptor.device_id),
            device_type: Some(pointerinjector::DeviceType::Touch),
            context: Some(pointerinjector::Context::View(context)),
            target: Some(pointerinjector::Target::View(target)),
            viewport,
            dispatch_policy: Some(pointerinjector::DispatchPolicy::TopHitAndAncestorsInTarget),
            scroll_v_range: None,
            scroll_h_range: None,
            buttons: None,
            ..Default::default()
        };

        // Keep track of the injector.
        self.mutable_state.borrow_mut().injectors.insert(touch_descriptor.device_id, device_proxy);

        // Register the new injector.
        self.injector_registry_proxy
            .register(config, device_server)
            .await
            .context("Failed to register injector.")?;
        log::info!("Registered injector with device id {:?}", touch_descriptor.device_id);

        Ok(())
    }

    /// Sends the given event to Scenic.
    ///
    /// # Parameters
    /// - `touch_event`: The touch event to send to Scenic.
    /// - `touch_descriptor`: The descriptor for the device that sent the touch event.
    /// - `event_time`: The time when the event was first recorded.
    async fn send_event_to_scenic(
        &self,
        touch_event: &touch_binding::TouchScreenEvent,
        touch_descriptor: &touch_binding::TouchScreenDeviceDescriptor,
        event_time: zx::MonotonicInstant,
    ) -> Result<(), anyhow::Error> {
        // The order in which events are sent to clients.
        let ordered_phases = vec![
            pointerinjector::EventPhase::Add,
            pointerinjector::EventPhase::Change,
            pointerinjector::EventPhase::Remove,
        ];

        // Make the trace duration end on the call to injector.inject, not the call's return.
        // The duration should start before the flow_begin is minted in
        // create_pointer_sample_event, and it should not include the injector.inject() call's
        // return from await.
        fuchsia_trace::duration_begin!(c"input", c"touch-inject-into-scenic");

        let mut events: Vec<pointerinjector::Event> = vec![];
        for phase in ordered_phases {
            let contacts: Vec<touch_binding::TouchContact> = touch_event
                .injector_contacts
                .get(&phase)
                .map_or(vec![], |contacts| contacts.to_owned());
            let new_events = contacts.into_iter().map(|contact| {
                Self::create_pointer_sample_event(
                    phase,
                    &contact,
                    touch_descriptor,
                    &self.display_size,
                    event_time,
                )
            });
            events.extend(new_events);
        }

        let injector =
            self.mutable_state.borrow().injectors.get(&touch_descriptor.device_id).cloned();
        if let Some(injector) = injector {
            let fut = injector.inject(&events);
            // This trace duration ends before awaiting on the returned future.
            fuchsia_trace::duration_end!(c"input", c"touch-inject-into-scenic");
            let _ = fut.await;
            Ok(())
        } else {
            fuchsia_trace::duration_end!(c"input", c"touch-inject-into-scenic");
            Err(anyhow::format_err!(
                "No injector found for touch device {}.",
                touch_descriptor.device_id
            ))
        }
    }

    /// Creates a [`fidl_fuchsia_ui_pointerinjector::Event`] representing the given touch contact.
    ///
    /// # Parameters
    /// - `phase`: The phase of the touch contact.
    /// - `contact`: The touch contact to create the event for.
    /// - `touch_descriptor`: The device descriptor for the device that generated the event.
    /// - `display_size`: The size of the associated touch display.
    /// - `event_time`: The time in nanoseconds when the event was first recorded.
    fn create_pointer_sample_event(
        phase: pointerinjector::EventPhase,
        contact: &touch_binding::TouchContact,
        touch_descriptor: &touch_binding::TouchScreenDeviceDescriptor,
        display_size: &Size,
        event_time: zx::MonotonicInstant,
    ) -> pointerinjector::Event {
        let position =
            Self::display_coordinate_from_contact(&contact, &touch_descriptor, display_size);
        let pointer_sample = pointerinjector::PointerSample {
            pointer_id: Some(contact.id),
            phase: Some(phase),
            position_in_viewport: Some([position.x, position.y]),
            scroll_v: None,
            scroll_h: None,
            pressed_buttons: None,
            ..Default::default()
        };
        let data = pointerinjector::Data::PointerSample(pointer_sample);

        let trace_flow_id = fuchsia_trace::Id::random();
        let event = pointerinjector::Event {
            timestamp: Some(event_time.into_nanos()),
            data: Some(data),
            trace_flow_id: Some(trace_flow_id.into()),
            ..Default::default()
        };

        fuchsia_trace::flow_begin!(c"input", c"dispatch_event_to_scenic", trace_flow_id);

        event
    }

    /// Converts an input event touch to a display coordinate, which is the coordinate space in
    /// which Scenic handles events.
    ///
    /// The display coordinate is calculated by normalizing the contact position to the display
    /// size. It does not account for the viewport position, which Scenic handles directly.
    ///
    /// # Parameters
    /// - `contact`: The contact to get the display coordinate from.
    /// - `touch_descriptor`: The device descriptor for the device that generated the event.
    ///                       This is used to compute the device coordinate.
    ///
    /// # Returns
    /// (x, y) coordinates.
    fn display_coordinate_from_contact(
        contact: &touch_binding::TouchContact,
        touch_descriptor: &touch_binding::TouchScreenDeviceDescriptor,
        display_size: &Size,
    ) -> Position {
        if let Some(contact_descriptor) = touch_descriptor.contacts.first() {
            // Scale the x position.
            let x_range: f32 =
                contact_descriptor.x_range.max as f32 - contact_descriptor.x_range.min as f32;
            let x_wrt_range: f32 = contact.position.x - contact_descriptor.x_range.min as f32;
            let x: f32 = (display_size.width * x_wrt_range) / x_range;

            // Scale the y position.
            let y_range: f32 =
                contact_descriptor.y_range.max as f32 - contact_descriptor.y_range.min as f32;
            let y_wrt_range: f32 = contact.position.y - contact_descriptor.y_range.min as f32;
            let y: f32 = (display_size.height * y_wrt_range) / y_range;

            Position { x, y }
        } else {
            return contact.position;
        }
    }

    /// Watches for viewport updates from the scene manager.
    pub async fn watch_viewport(self: Rc<Self>) {
        let configuration_proxy = self.configuration_proxy.clone();
        let mut viewport_stream = HangingGetStream::new(
            configuration_proxy,
            pointerinjector_config::SetupProxy::watch_viewport,
        );
        loop {
            match viewport_stream.next().await {
                Some(Ok(new_viewport)) => {
                    // Update the viewport tracked by this handler.
                    self.mutable_state.borrow_mut().viewport = Some(new_viewport.clone());

                    // Update Scenic with the latest viewport.
                    let injectors: Vec<pointerinjector::DeviceProxy> =
                        self.mutable_state.borrow_mut().injectors.values().cloned().collect();
                    for injector in injectors {
                        let events = &[pointerinjector::Event {
                            timestamp: Some(fuchsia_async::MonotonicInstant::now().into_nanos()),
                            data: Some(pointerinjector::Data::Viewport(new_viewport.clone())),
                            trace_flow_id: Some(fuchsia_trace::Id::random().into()),
                            ..Default::default()
                        }];
                        injector.inject(events).await.expect("Failed to inject updated viewport.");
                    }
                }
                Some(Err(e)) => {
                    self.metrics_logger.log_error(
                        InputPipelineErrorMetricDimensionEvent::TouchInjectorErrorWhileReadingViewportUpdate,
                        std::format!("Error while reading viewport update: {}", e));
                    return;
                }
                None => {
                    self.metrics_logger.log_error(
                        InputPipelineErrorMetricDimensionEvent::TouchInjectorViewportUpdateStreamTerminatedUnexpectedly,
                        "Viewport update stream terminated unexpectedly");
                    return;
                }
            }
        }
    }

    /// Creates a fidl_ui_input::TouchButtonsEvent from a touch_binding::TouchScreenEvent.
    ///
    /// # Parameters
    /// - `event`: The TouchScreenEvent to create a TouchButtonsEvent from.
    /// - `event_time`: The time when the event was first recorded.
    /// - `touch_descriptor`: The descriptor of the new touch device.
    fn create_touch_buttons_event(
        event: &touch_binding::TouchScreenEvent,
        event_time: zx::MonotonicInstant,
        touch_descriptor: &touch_binding::TouchScreenDeviceDescriptor,
    ) -> fidl_ui_input::TouchButtonsEvent {
        let pressed_buttons = match event.pressed_buttons.len() {
            0 => None,
            _ => Some(
                event
                    .pressed_buttons
                    .clone()
                    .into_iter()
                    .map(|button| match button {
                        fidl_input_report::TouchButton::Palm => fidl_ui_input::TouchButton::Palm,
                        fidl_input_report::TouchButton::__SourceBreaking { unknown_ordinal: n } => {
                            fidl_ui_input::TouchButton::__SourceBreaking {
                                unknown_ordinal: n as u32,
                            }
                        }
                    })
                    .collect::<Vec<_>>(),
            ),
        };
        fidl_ui_input::TouchButtonsEvent {
            event_time: Some(event_time),
            device_info: Some(fidl_ui_input::TouchDeviceInfo {
                id: Some(touch_descriptor.device_id),
                ..Default::default()
            }),
            pressed_buttons,
            ..Default::default()
        }
    }

    /// Sends touch button events to touch button listeners.
    ///
    /// # Parameters
    /// - `event`: The event to send to the listeners.
    async fn send_event_to_listeners(self: &Rc<Self>, event: &fidl_ui_input::TouchButtonsEvent) {
        let tracker = &self.mutable_state.borrow().send_event_task_tracker;

        for (handle, listener) in &self.mutable_state.borrow().listeners {
            let weak_handler = Rc::downgrade(&self);
            let listener_clone = listener.clone();
            let handle_clone = handle.clone();
            let event_to_send = event.clone();
            let fut = async move {
                match listener_clone.on_event(&event_to_send).await {
                    Ok(_) => {}
                    Err(e) => {
                        if let Some(handler) = weak_handler.upgrade() {
                            handler.mutable_state.borrow_mut().listeners.remove(&handle_clone);
                            log::info!(
                                "Unregistering listener; unable to send TouchButtonsEvent: {:?}",
                                e
                            )
                        }
                    }
                }
            };

            let metrics_logger_clone = self.metrics_logger.clone();
            tracker.track(metrics_logger_clone, fasync::Task::local(fut));
        }
    }

    // Add the listener to the registry.
    ///
    /// # Parameters
    /// - `proxy`: A new listener proxy to send events to.
    pub async fn register_listener_proxy(
        self: &Rc<Self>,
        proxy: fidl_ui_policy::TouchButtonsListenerProxy,
    ) {
        self.mutable_state
            .borrow_mut()
            .listeners
            .insert(proxy.as_channel().raw_handle(), proxy.clone());

        // Send the listener the last touch button event.
        if let Some(event) = &self.mutable_state.borrow().last_button_event {
            let event_to_send = event.clone();
            let fut = async move {
                match proxy.on_event(&event_to_send).await {
                    Ok(_) => {}
                    Err(e) => {
                        log::info!("Failed to send touch buttons event to listener {:?}", e)
                    }
                }
            };
            let metrics_logger_clone = self.metrics_logger.clone();
            self.mutable_state
                .borrow()
                .send_event_task_tracker
                .track(metrics_logger_clone, fasync::Task::local(fut));
        }
    }
}

/// Maintains a collection of pending local [`Task`]s, allowing them to be dropped (and cancelled)
/// en masse.
#[derive(Debug)]
pub struct LocalTaskTracker {
    sender: mpsc::UnboundedSender<fasync::Task<()>>,
    _receiver_task: fasync::Task<()>,
}

impl LocalTaskTracker {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded();
        let receiver_task = fasync::Task::local(async move {
            // Drop the tasks as they are completed.
            receiver.for_each_concurrent(None, |task: fasync::Task<()>| task).await
        });

        Self { sender, _receiver_task: receiver_task }
    }

    /// Submits a new task to track.
    pub fn track(&self, metrics_logger: metrics::MetricsLogger, task: fasync::Task<()>) {
        match self.sender.unbounded_send(task) {
            Ok(_) => {}
            // `Full` should never happen because this is unbounded.
            // `Disconnected` might happen if the `Service` was dropped. However, it's not clear how
            // to create such a race condition.
            Err(e) => {
                metrics_logger.log_error(
                    InputPipelineErrorMetricDimensionEvent::TouchFailedToSendTouchScreenEvent,
                    std::format!("Unexpected {e:?} while pushing task"),
                );
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_handler::InputHandler;
    use crate::testing_utilities::{
        create_fake_input_event, create_touch_contact, create_touch_pointer_sample_event,
        create_touch_screen_event, create_touch_screen_event_with_handled, create_touchpad_event,
        get_touch_screen_device_descriptor,
    };
    use assert_matches::assert_matches;
    use futures::{FutureExt, TryStreamExt};
    use maplit::hashmap;
    use pretty_assertions::assert_eq;
    use std::collections::HashSet;
    use std::convert::TryFrom as _;
    use std::ops::Add;
    use {
        fidl_fuchsia_input_report as fidl_input_report, fidl_fuchsia_ui_input as fidl_ui_input,
        fidl_fuchsia_ui_policy as fidl_ui_policy, fuchsia_async as fasync,
    };

    const TOUCH_ID: u32 = 1;
    const DISPLAY_WIDTH: f32 = 100.0;
    const DISPLAY_HEIGHT: f32 = 100.0;

    struct TestFixtures {
        touch_handler: Rc<TouchInjectorHandler>,
        device_listener_proxy: fidl_ui_policy::DeviceListenerRegistryProxy,
        injector_registry_request_stream: pointerinjector::RegistryRequestStream,
        configuration_request_stream: pointerinjector_config::SetupRequestStream,
        inspector: fuchsia_inspect::Inspector,
        _test_node: fuchsia_inspect::Node,
    }

    fn spawn_device_listener_registry_server(
        handler: Rc<TouchInjectorHandler>,
    ) -> fidl_ui_policy::DeviceListenerRegistryProxy {
        let (device_listener_proxy, mut device_listener_stream) =
            fidl::endpoints::create_proxy_and_stream::<fidl_ui_policy::DeviceListenerRegistryMarker>(
            );

        fasync::Task::local(async move {
            loop {
                match device_listener_stream.try_next().await {
                    Ok(Some(
                        fidl_ui_policy::DeviceListenerRegistryRequest::RegisterTouchButtonsListener {
                            listener,
                            responder,
                        },
                    )) => {
                        handler.register_listener_proxy(listener.into_proxy()).await;
                        let _ = responder.send();
                    }
                    Ok(Some(_)) => {
                        panic!("Unexpected registration");
                    }
                    Ok(None) => {
                        break;
                    }
                    Err(e) => {
                        panic!("Error handling device listener registry request stream: {}", e);
                    }
                }
            }
        })
        .detach();

        device_listener_proxy
    }

    impl TestFixtures {
        async fn new() -> Self {
            let inspector = fuchsia_inspect::Inspector::default();
            let test_node = inspector.root().create_child("test_node");
            let (configuration_proxy, mut configuration_request_stream) =
                fidl::endpoints::create_proxy_and_stream::<pointerinjector_config::SetupMarker>();
            let (injector_registry_proxy, injector_registry_request_stream) =
                fidl::endpoints::create_proxy_and_stream::<pointerinjector::RegistryMarker>();

            let touch_handler_fut = TouchInjectorHandler::new_handler(
                configuration_proxy,
                injector_registry_proxy,
                Size { width: DISPLAY_WIDTH, height: DISPLAY_HEIGHT },
                &test_node,
                metrics::MetricsLogger::default(),
            );

            let handle_initial_request_fut = async {
                match configuration_request_stream.next().await {
                    Some(Ok(pointerinjector_config::SetupRequest::GetViewRefs {
                        responder,
                        ..
                    })) => {
                        let context = fuchsia_scenic::ViewRefPair::new()
                            .expect("Failed to create viewrefpair.")
                            .view_ref;
                        let target = fuchsia_scenic::ViewRefPair::new()
                            .expect("Failed to create viewrefpair.")
                            .view_ref;
                        let _ = responder.send(context, target);
                    }
                    other => panic!("Expected GetViewRefs request, got {:?}", other),
                }
            };

            let (touch_handler_res, _) =
                futures::future::join(touch_handler_fut, handle_initial_request_fut).await;

            let touch_handler = touch_handler_res.expect("Failed to create touch handler.");
            let device_listener_proxy =
                spawn_device_listener_registry_server(touch_handler.clone());

            TestFixtures {
                touch_handler,
                device_listener_proxy,
                injector_registry_request_stream,
                configuration_request_stream,
                inspector,
                _test_node: test_node,
            }
        }
    }

    /// Returns an |input_device::InputDeviceDescriptor::Touchpad|.
    fn get_touchpad_device_descriptor() -> input_device::InputDeviceDescriptor {
        input_device::InputDeviceDescriptor::Touchpad(touch_binding::TouchpadDeviceDescriptor {
            device_id: 1,
            contacts: vec![touch_binding::ContactDeviceDescriptor {
                x_range: fidl_input_report::Range { min: 0, max: 100 },
                y_range: fidl_input_report::Range { min: 0, max: 100 },
                x_unit: fidl_input_report::Unit {
                    type_: fidl_input_report::UnitType::Meters,
                    exponent: -6,
                },
                y_unit: fidl_input_report::Unit {
                    type_: fidl_input_report::UnitType::Meters,
                    exponent: -6,
                },
                pressure_range: None,
                width_range: None,
                height_range: None,
            }],
        })
    }

    /// Handles |fidl_fuchsia_pointerinjector::DeviceRequest|s by asserting the `injector_stream`
    /// gets `expected_event`.
    async fn handle_device_request_stream(
        mut injector_stream: pointerinjector::DeviceRequestStream,
        expected_event: pointerinjector::Event,
    ) {
        match injector_stream.next().await {
            Some(Ok(pointerinjector::DeviceRequest::Inject { events, responder })) => {
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].timestamp, expected_event.timestamp);
                assert_eq!(events[0].data, expected_event.data);
                responder.send().expect("failed to respond");
            }
            Some(Err(e)) => panic!("FIDL error {}", e),
            None => panic!("Expected another event."),
        }
    }

    // Creates a |pointerinjector::Viewport|.
    fn create_viewport(min: f32, max: f32) -> pointerinjector::Viewport {
        pointerinjector::Viewport {
            extents: Some([[min, min], [max, max]]),
            viewport_to_context_transform: None,
            ..Default::default()
        }
    }

    #[fuchsia::test]
    async fn events_with_pressed_buttons_are_sent_to_listener() {
        let fixtures = TestFixtures::new().await;
        let (listener, mut listener_stream) =
            fidl::endpoints::create_request_stream::<fidl_ui_policy::TouchButtonsListenerMarker>();
        fixtures
            .device_listener_proxy
            .register_touch_buttons_listener(listener)
            .await
            .expect("Failed to register listener.");

        let descriptor = get_touch_screen_device_descriptor();
        let event_time = zx::MonotonicInstant::get();
        let input_event = create_touch_screen_event(hashmap! {}, event_time, &descriptor);

        let _ = fixtures.touch_handler.clone().handle_input_event(input_event).await;

        let expected_touch_buttons_event = fidl_ui_input::TouchButtonsEvent {
            event_time: Some(event_time),
            device_info: Some(fidl_ui_input::TouchDeviceInfo { id: Some(1), ..Default::default() }),
            ..Default::default()
        };

        assert_matches!(
            listener_stream.next().await,
            Some(Ok(fidl_ui_policy::TouchButtonsListenerRequest::OnEvent {
                event,
                responder,
            })) => {
                assert_eq!(event, expected_touch_buttons_event);
                let _ = responder.send();
            }
        );
    }

    #[fuchsia::test]
    async fn events_with_contacts_are_not_sent_to_listener() {
        let fixtures = TestFixtures::new().await;
        let (listener, mut listener_stream) =
            fidl::endpoints::create_request_stream::<fidl_ui_policy::TouchButtonsListenerMarker>();
        fixtures
            .device_listener_proxy
            .register_touch_buttons_listener(listener)
            .await
            .expect("Failed to register listener.");

        let descriptor = get_touch_screen_device_descriptor();
        let event_time = zx::MonotonicInstant::get();
        let contact = create_touch_contact(TOUCH_ID, Position { x: 20.0, y: 40.0 });
        let input_event = create_touch_screen_event(
            hashmap! {
                fidl_ui_input::PointerEventPhase::Add
                    => vec![contact.clone()],
            },
            event_time,
            &descriptor,
        );

        let _ = fixtures.touch_handler.clone().handle_input_event(input_event).await;

        assert!(listener_stream.next().now_or_never().is_none());
    }

    #[fuchsia::test]
    async fn multiple_listeners_receive_pressed_button_events() {
        let fixtures = TestFixtures::new().await;
        let (first_listener, mut first_listener_stream) =
            fidl::endpoints::create_request_stream::<fidl_ui_policy::TouchButtonsListenerMarker>();
        let (second_listener, mut second_listener_stream) =
            fidl::endpoints::create_request_stream::<fidl_ui_policy::TouchButtonsListenerMarker>();
        fixtures
            .device_listener_proxy
            .register_touch_buttons_listener(first_listener)
            .await
            .expect("Failed to register listener.");
        fixtures
            .device_listener_proxy
            .register_touch_buttons_listener(second_listener)
            .await
            .expect("Failed to register listener.");

        let descriptor = get_touch_screen_device_descriptor();
        let event_time = zx::MonotonicInstant::get();
        let input_event = create_touch_screen_event(hashmap! {}, event_time, &descriptor);

        let _ = fixtures.touch_handler.clone().handle_input_event(input_event).await;

        let expected_touch_buttons_event = fidl_ui_input::TouchButtonsEvent {
            event_time: Some(event_time),
            device_info: Some(fidl_ui_input::TouchDeviceInfo { id: Some(1), ..Default::default() }),
            ..Default::default()
        };

        assert_matches!(
            first_listener_stream.next().await,
            Some(Ok(fidl_ui_policy::TouchButtonsListenerRequest::OnEvent {
                event,
                responder,
            })) => {
                assert_eq!(event, expected_touch_buttons_event);
                let _ = responder.send();
            }
        );
        assert_matches!(
            second_listener_stream.next().await,
            Some(Ok(fidl_ui_policy::TouchButtonsListenerRequest::OnEvent {
                event,
                responder,
            })) => {
                assert_eq!(event, expected_touch_buttons_event);
                let _ = responder.send();
            }
        );
    }

    // Tests that TouchInjectorHandler::watch_viewport() tracks viewport updates and notifies
    // injectors about said updates.
    #[fuchsia::test]
    async fn receives_viewport_updates() {
        let mut fixtures = TestFixtures::new().await;

        // Add an injector.
        let (injector_device_proxy, mut injector_device_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<pointerinjector::DeviceMarker>();
        fixtures
            .touch_handler
            .mutable_state
            .borrow_mut()
            .injectors
            .insert(1, injector_device_proxy);

        // This nested block is used to bound the lifetime of `watch_viewport_fut`.
        {
            // Request a viewport update.
            let _watch_viewport_task =
                fasync::Task::local(fixtures.touch_handler.clone().watch_viewport());

            // Send a viewport update.
            match fixtures.configuration_request_stream.next().await {
                Some(Ok(pointerinjector_config::SetupRequest::WatchViewport {
                    responder, ..
                })) => {
                    responder.send(&create_viewport(0.0, 100.0)).expect("Failed to send viewport.");
                }
                other => panic!("Received unexpected value: {:?}", other),
            };

            // Check that the injector received an updated viewport
            match injector_device_request_stream.next().await {
                Some(Ok(pointerinjector::DeviceRequest::Inject { events, responder })) => {
                    assert_eq!(events.len(), 1);
                    assert!(events[0].data.is_some());
                    assert_eq!(
                        events[0].data,
                        Some(pointerinjector::Data::Viewport(create_viewport(0.0, 100.0)))
                    );
                    responder.send().expect("injector stream failed to respond.");
                }
                other => panic!("Received unexpected value: {:?}", other),
            }

            // Request viewport update.
            // Send viewport update.
            match fixtures.configuration_request_stream.next().await {
                Some(Ok(pointerinjector_config::SetupRequest::WatchViewport {
                    responder, ..
                })) => {
                    responder
                        .send(&create_viewport(100.0, 200.0))
                        .expect("Failed to send viewport.");
                }
                other => panic!("Received unexpected value: {:?}", other),
            };

            // Check that the injector received an updated viewport
            match injector_device_request_stream.next().await {
                Some(Ok(pointerinjector::DeviceRequest::Inject { events, responder })) => {
                    assert_eq!(events.len(), 1);
                    assert!(events[0].data.is_some());
                    assert_eq!(
                        events[0].data,
                        Some(pointerinjector::Data::Viewport(create_viewport(100.0, 200.0)))
                    );
                    responder.send().expect("injector stream failed to respond.");
                }
                other => panic!("Received unexpected value: {:?}", other),
            }
        }

        // Check the viewport on the handler is accurate.
        let expected_viewport = create_viewport(100.0, 200.0);
        assert_eq!(fixtures.touch_handler.mutable_state.borrow().viewport, Some(expected_viewport));
    }

    // Tests that an add contact event is dropped without a viewport.
    #[fuchsia::test]
    async fn add_contact_drops_without_viewport() {
        let mut fixtures = TestFixtures::new().await;

        // Create touch event.
        let event_time = zx::MonotonicInstant::get();
        let contact = create_touch_contact(TOUCH_ID, Position { x: 20.0, y: 40.0 });
        let descriptor = get_touch_screen_device_descriptor();
        let input_event = input_device::UnhandledInputEvent::try_from(create_touch_screen_event(
            hashmap! {
                fidl_ui_input::PointerEventPhase::Add
                    => vec![contact.clone()],
            },
            event_time,
            &descriptor,
        ))
        .unwrap();

        // Clear the viewport that was set during test fixture setup.
        fixtures.touch_handler.mutable_state.borrow_mut().viewport = None;

        // Try to handle the event.
        let _ = fixtures.touch_handler.clone().handle_unhandled_input_event(input_event).await;

        // Injector should not receive anything because the handler has no viewport.
        assert!(fixtures.injector_registry_request_stream.next().now_or_never().is_none());
    }

    // Tests that an add contact event is handled correctly with a viewport.
    #[fuchsia::test]
    async fn add_contact_succeeds_with_viewport() {
        let mut fixtures = TestFixtures::new().await;

        // Add an injector.
        let (injector_device_proxy, mut injector_device_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<pointerinjector::DeviceMarker>();
        fixtures
            .touch_handler
            .mutable_state
            .borrow_mut()
            .injectors
            .insert(1, injector_device_proxy);

        // Request a viewport update.
        let _watch_viewport_task =
            fasync::Task::local(fixtures.touch_handler.clone().watch_viewport());

        // Send a viewport update.
        match fixtures.configuration_request_stream.next().await {
            Some(Ok(pointerinjector_config::SetupRequest::WatchViewport { responder, .. })) => {
                responder.send(&create_viewport(0.0, 100.0)).expect("Failed to send viewport.");
            }
            other => panic!("Received unexpected value: {:?}", other),
        };

        // Check that the injector received an updated viewport
        match injector_device_request_stream.next().await {
            Some(Ok(pointerinjector::DeviceRequest::Inject { events, responder })) => {
                assert_eq!(events.len(), 1);
                assert!(events[0].data.is_some());
                assert_eq!(
                    events[0].data,
                    Some(pointerinjector::Data::Viewport(create_viewport(0.0, 100.0)))
                );
                responder.send().expect("injector stream failed to respond.");
            }
            other => panic!("Received unexpected value: {:?}", other),
        }

        // Create touch event.
        let event_time = zx::MonotonicInstant::get();
        let contact = create_touch_contact(TOUCH_ID, Position { x: 20.0, y: 40.0 });
        let descriptor = get_touch_screen_device_descriptor();
        let input_event = input_device::UnhandledInputEvent::try_from(create_touch_screen_event(
            hashmap! {
                fidl_ui_input::PointerEventPhase::Add
                    => vec![contact.clone()],
            },
            event_time,
            &descriptor,
        ))
        .unwrap();

        // Handle event.
        let handle_event_fut =
            fixtures.touch_handler.clone().handle_unhandled_input_event(input_event);

        // Declare expected event.
        let expected_event = create_touch_pointer_sample_event(
            pointerinjector::EventPhase::Add,
            &contact,
            Position { x: 20.0, y: 40.0 },
            event_time,
        );

        // Await all futures concurrently. If this completes, then the touch event was handled and
        // matches `expected_event`.
        let device_fut =
            handle_device_request_stream(injector_device_request_stream, expected_event);
        let (handle_result, _) = futures::future::join(handle_event_fut, device_fut).await;

        // No unhandled events.
        assert_matches!(
            handle_result.as_slice(),
            [input_device::InputEvent { handled: input_device::Handled::Yes, .. }]
        );
    }

    // Tests that an add touchpad contact event with viewport is unhandled and not send to scenic.
    #[fuchsia::test]
    async fn add_touchpad_contact_with_viewport() {
        let mut fixtures = TestFixtures::new().await;

        // Add an injector.
        let (injector_device_proxy, mut injector_device_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<pointerinjector::DeviceMarker>();
        fixtures
            .touch_handler
            .mutable_state
            .borrow_mut()
            .injectors
            .insert(1, injector_device_proxy);

        // Request a viewport update.
        let _watch_viewport_task =
            fasync::Task::local(fixtures.touch_handler.clone().watch_viewport());

        // Send a viewport update.
        match fixtures.configuration_request_stream.next().await {
            Some(Ok(pointerinjector_config::SetupRequest::WatchViewport { responder, .. })) => {
                responder.send(&create_viewport(0.0, 100.0)).expect("Failed to send viewport.");
            }
            other => panic!("Received unexpected value: {:?}", other),
        };

        // Check that the injector received an updated viewport
        match injector_device_request_stream.next().await {
            Some(Ok(pointerinjector::DeviceRequest::Inject { events, responder })) => {
                assert_eq!(events.len(), 1);
                assert!(events[0].data.is_some());
                assert_eq!(
                    events[0].data,
                    Some(pointerinjector::Data::Viewport(create_viewport(0.0, 100.0)))
                );
                responder.send().expect("injector stream failed to respond.");
            }
            other => panic!("Received unexpected value: {:?}", other),
        }

        // Create touch event.
        let event_time = zx::MonotonicInstant::get();
        let contact = create_touch_contact(TOUCH_ID, Position { x: 20.0, y: 40.0 });
        let descriptor = get_touchpad_device_descriptor();
        let input_event = input_device::UnhandledInputEvent::try_from(create_touchpad_event(
            vec![contact.clone()],
            HashSet::new(),
            event_time,
            &descriptor,
        ))
        .unwrap();

        // Handle event.
        let handle_event_fut =
            fixtures.touch_handler.clone().handle_unhandled_input_event(input_event);

        let handle_result = handle_event_fut.await;

        // Event is not handled.
        assert_matches!(
            handle_result.as_slice(),
            [input_device::InputEvent { handled: input_device::Handled::No, .. }]
        );

        // Injector should not receive anything because the handler does not support touchpad yet.
        assert!(fixtures.injector_registry_request_stream.next().now_or_never().is_none());
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn touch_injector_handler_initialized_with_inspect_node() {
        let fixtures = TestFixtures::new().await;
        diagnostics_assertions::assert_data_tree!(fixtures.inspector, root: {
            test_node: {
                touch_injector_handler: {
                    events_received_count: 0u64,
                    events_handled_count: 0u64,
                    last_received_timestamp_ns: 0u64,
                    "fuchsia.inspect.Health": {
                        status: "STARTING_UP",
                        // Timestamp value is unpredictable and not relevant in this context,
                        // so we only assert that the property is present.
                        start_timestamp_nanos: diagnostics_assertions::AnyProperty
                    },
                }
            }
        });
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn touch_injector_handler_inspect_counts_events() {
        let fixtures = TestFixtures::new().await;

        let contact = create_touch_contact(TOUCH_ID, Position { x: 20.0, y: 40.0 });
        let descriptor = get_touch_screen_device_descriptor();
        let event_time1 = zx::MonotonicInstant::get();
        let event_time2 = event_time1.add(zx::MonotonicDuration::from_micros(1));
        let event_time3 = event_time2.add(zx::MonotonicDuration::from_micros(1));

        let input_events = vec![
            create_touch_screen_event(
                hashmap! {
                    fidl_ui_input::PointerEventPhase::Add
                        => vec![contact.clone()],
                },
                event_time1,
                &descriptor,
            ),
            create_touch_screen_event(
                hashmap! {
                    fidl_ui_input::PointerEventPhase::Move
                        => vec![contact.clone()],
                },
                event_time2,
                &descriptor,
            ),
            // Should not count non-touch input event.
            create_fake_input_event(event_time2),
            // Should not count received event that has already been handled.
            create_touch_screen_event_with_handled(
                hashmap! {
                    fidl_ui_input::PointerEventPhase::Move
                        => vec![contact.clone()],
                },
                event_time2,
                &descriptor,
                input_device::Handled::Yes,
            ),
            create_touch_screen_event(
                hashmap! {
                    fidl_ui_input::PointerEventPhase::Remove
                        => vec![contact.clone()],
                },
                event_time3,
                &descriptor,
            ),
        ];

        for input_event in input_events {
            fixtures.touch_handler.clone().handle_input_event(input_event).await;
        }

        let last_received_event_time: u64 = event_time3.into_nanos().try_into().unwrap();

        diagnostics_assertions::assert_data_tree!(fixtures.inspector, root: {
            test_node: {
                touch_injector_handler: {
                    events_received_count: 3u64,
                    events_handled_count: 3u64,
                    last_received_timestamp_ns: last_received_event_time,
                    "fuchsia.inspect.Health": {
                        status: "STARTING_UP",
                        // Timestamp value is unpredictable and not relevant in this context,
                        // so we only assert that the property is present.
                        start_timestamp_nanos: diagnostics_assertions::AnyProperty
                    },
                }
            }
        });
    }
}
