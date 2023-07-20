// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::input_device::{self, Handled, InputDeviceBinding, InputDeviceStatus, InputEvent},
    crate::metrics,
    crate::mouse_binding,
    crate::utils::{Position, Size},
    anyhow::{format_err, Context, Error},
    async_trait::async_trait,
    fidl_fuchsia_input_report as fidl_input_report,
    fidl_fuchsia_input_report::{InputDeviceProxy, InputReport},
    fidl_fuchsia_ui_input as fidl_ui_input, fidl_fuchsia_ui_pointerinjector as pointerinjector,
    fuchsia_inspect::{health::Reporter, ArrayProperty},
    fuchsia_zircon as zx,
    futures::channel::mpsc::{UnboundedReceiver, UnboundedSender},
    maplit::hashmap,
    metrics_registry::*,
    std::collections::HashMap,
    std::collections::HashSet,
    std::iter::FromIterator,
};

/// A [`TouchScreenEvent`] represents a set of contacts and the phase those contacts are in.
///
/// For example, when a user touches a touch screen with two fingers, there will be two
/// [`TouchContact`]s. When a user removes one finger, there will still be two contacts
/// but one will be reported as removed.
///
/// The expected sequence for any given contact is:
/// 1. [`fidl_fuchsia_ui_input::PointerEventPhase::Add`]
/// 2. [`fidl_fuchsia_ui_input::PointerEventPhase::Down`]
/// 3. 0 or more [`fidl_fuchsia_ui_input::PointerEventPhase::Move`]
/// 4. [`fidl_fuchsia_ui_input::PointerEventPhase::Up`]
/// 5. [`fidl_fuchsia_ui_input::PointerEventPhase::Remove`]
///
/// Additionally, a [`fidl_fuchsia_ui_input::PointerEventPhase::Cancel`] may be sent at any time
/// signalling that the event is no longer directed towards the receiver.
#[derive(Clone, Debug, PartialEq)]
pub struct TouchScreenEvent {
    /// Deprecated. To be removed with fxbug.dev/75817.
    /// The contacts associated with the touch event. For example, a two-finger touch would result
    /// in one touch event with two [`TouchContact`]s.
    ///
    /// Contacts are grouped based on their current phase (e.g., down, move).
    pub contacts: HashMap<fidl_ui_input::PointerEventPhase, Vec<TouchContact>>,

    /// The contacts associated with the touch event. For example, a two-finger touch would result
    /// in one touch event with two [`TouchContact`]s.
    ///
    /// Contacts are grouped based on their current phase (e.g., add, change).
    pub injector_contacts: HashMap<pointerinjector::EventPhase, Vec<TouchContact>>,
}

impl TouchScreenEvent {
    pub fn record_inspect(&self, node: &fuchsia_inspect::Node) {
        let contacts_clone = self.injector_contacts.clone();
        node.record_child("injector_contacts", move |contacts_node| {
            for (phase, contacts) in contacts_clone.iter() {
                let phase_str = match phase {
                    pointerinjector::EventPhase::Add => "add",
                    pointerinjector::EventPhase::Change => "change",
                    pointerinjector::EventPhase::Remove => "remove",
                    pointerinjector::EventPhase::Cancel => "cancel",
                };
                contacts_node.record_child(phase_str, move |phase_node| {
                    for contact in contacts.iter() {
                        phase_node.record_child(contact.id.to_string(), move |contact_node| {
                            contact_node
                                .record_double("position_x_mm", f64::from(contact.position.x));
                            contact_node
                                .record_double("position_y_mm", f64::from(contact.position.y));
                            if let Some(pressure) = contact.pressure {
                                contact_node.record_int("pressure", pressure);
                            }
                            if let Some(contact_size) = contact.contact_size {
                                contact_node.record_double(
                                    "contact_width_mm",
                                    f64::from(contact_size.width),
                                );
                                contact_node.record_double(
                                    "contact_height_mm",
                                    f64::from(contact_size.height),
                                );
                            }
                        });
                    }
                });
            }
        });
    }
}

/// A [`TouchpadEvent`] represents a set of contacts.
///
/// For example, when a user touches a touch screen with two fingers, there will be two
/// [`TouchContact`]s in the vector.
#[derive(Clone, Debug, PartialEq)]
pub struct TouchpadEvent {
    /// The contacts associated with the touch event. For example, a two-finger touch would result
    /// in one touch event with two [`TouchContact`]s.
    pub injector_contacts: Vec<TouchContact>,

    /// The complete button state including this event.
    pub pressed_buttons: HashSet<mouse_binding::MouseButton>,
}

impl TouchpadEvent {
    pub fn record_inspect(&self, node: &fuchsia_inspect::Node) {
        let pressed_buttons_node =
            node.create_uint_array("pressed_buttons", self.pressed_buttons.len());
        self.pressed_buttons.iter().enumerate().for_each(|(i, button)| {
            pressed_buttons_node.set(i, *button);
        });
        node.record(pressed_buttons_node);

        // Populate TouchpadEvent contact details.
        let contacts_clone = self.injector_contacts.clone();
        node.record_child("injector_contacts", move |contacts_node| {
            for contact in contacts_clone.iter() {
                contacts_node.record_child(contact.id.to_string(), move |contact_node| {
                    contact_node.record_double("position_x_mm", f64::from(contact.position.x));
                    contact_node.record_double("position_y_mm", f64::from(contact.position.y));
                    if let Some(pressure) = contact.pressure {
                        contact_node.record_int("pressure", pressure);
                    }
                    if let Some(contact_size) = contact.contact_size {
                        contact_node
                            .record_double("contact_width_mm", f64::from(contact_size.width));
                        contact_node
                            .record_double("contact_height_mm", f64::from(contact_size.height));
                    }
                })
            }
        });
    }
}

/// [`TouchDeviceType`] indicates the type of touch device. Both Touch Screen and Windows Precision
/// Touchpad send touch event from driver but need different process inside input pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TouchDeviceType {
    TouchScreen,
    WindowsPrecisionTouchpad,
}

/// A [`TouchContact`] represents a single contact (e.g., one touch of a multi-touch gesture) related
/// to a touch event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchContact {
    /// The identifier of the contact. Unique per touch device.
    pub id: u32,

    /// The position of the touch event, in the units of the associated
    /// [`ContactDeviceDescriptor`]'s `range`.
    pub position: Position,

    /// The pressure associated with the contact, in the units of the associated
    /// [`ContactDeviceDescriptor`]'s `pressure_range`.
    pub pressure: Option<i64>,

    /// The size of the touch event, in the units of the associated
    /// [`ContactDeviceDescriptor`]'s `range`.
    pub contact_size: Option<Size>,
}

impl Eq for TouchContact {}

impl From<&fidl_fuchsia_input_report::ContactInputReport> for TouchContact {
    fn from(fidl_contact: &fidl_fuchsia_input_report::ContactInputReport) -> TouchContact {
        let contact_size =
            if fidl_contact.contact_width.is_some() && fidl_contact.contact_height.is_some() {
                Some(Size {
                    width: fidl_contact.contact_width.unwrap() as f32,
                    height: fidl_contact.contact_height.unwrap() as f32,
                })
            } else {
                None
            };

        TouchContact {
            id: fidl_contact.contact_id.unwrap_or_default(),
            position: Position {
                x: fidl_contact.position_x.unwrap_or_default() as f32,
                y: fidl_contact.position_y.unwrap_or_default() as f32,
            },
            pressure: fidl_contact.pressure,
            contact_size,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TouchScreenDeviceDescriptor {
    /// The id of the connected touch screen input device.
    pub device_id: u32,

    /// The descriptors for the possible contacts associated with the device.
    pub contacts: Vec<ContactDeviceDescriptor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TouchpadDeviceDescriptor {
    /// The id of the connected touchpad input device.
    pub device_id: u32,

    /// The descriptors for the possible contacts associated with the device.
    pub contacts: Vec<ContactDeviceDescriptor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TouchDeviceDescriptor {
    TouchScreen(TouchScreenDeviceDescriptor),
    Touchpad(TouchpadDeviceDescriptor),
}

/// A [`ContactDeviceDescriptor`] describes the possible values touch contact properties can take on.
///
/// This descriptor can be used, for example, to determine where on a screen a touch made contact.
///
/// # Example
///
/// ```
/// // Determine the scaling factor between the display and the touch device's x range.
/// let scaling_factor =
///     display_width / (contact_descriptor._x_range.end - contact_descriptor._x_range.start);
/// // Use the scaling factor to scale the contact report's x position.
/// let hit_location =
///     scaling_factor * (contact_report.position_x - contact_descriptor._x_range.start);
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContactDeviceDescriptor {
    /// The range of possible x values for this touch contact.
    pub x_range: fidl_input_report::Range,

    /// The range of possible y values for this touch contact.
    pub y_range: fidl_input_report::Range,

    /// The unit of measure for `x_range`.
    pub x_unit: fidl_input_report::Unit,

    /// The unit of measure for `y_range`.
    pub y_unit: fidl_input_report::Unit,

    /// The range of possible pressure values for this touch contact.
    pub pressure_range: Option<fidl_input_report::Range>,

    /// The range of possible widths for this touch contact.
    pub width_range: Option<fidl_input_report::Range>,

    /// The range of possible heights for this touch contact.
    pub height_range: Option<fidl_input_report::Range>,
}

/// A [`TouchBinding`] represents a connection to a touch input device.
///
/// The [`TouchBinding`] parses and exposes touch descriptor properties (e.g., the range of
/// possible x values for touch contacts) for the device it is associated with.
/// It also parses [`InputReport`]s from the device, and sends them to the device binding owner over
/// `event_sender`.
pub struct TouchBinding {
    /// The channel to stream InputEvents to.
    event_sender: UnboundedSender<InputEvent>,

    /// Holds information about this device.
    device_descriptor: TouchDeviceDescriptor,

    /// Touch device type of the touch device.
    touch_device_type: TouchDeviceType,

    /// Proxy to the device.
    device_proxy: InputDeviceProxy,
}

#[async_trait]
impl input_device::InputDeviceBinding for TouchBinding {
    fn input_event_sender(&self) -> UnboundedSender<InputEvent> {
        self.event_sender.clone()
    }

    fn get_device_descriptor(&self) -> input_device::InputDeviceDescriptor {
        match self.device_descriptor.clone() {
            TouchDeviceDescriptor::TouchScreen(desc) => {
                input_device::InputDeviceDescriptor::TouchScreen(desc)
            }
            TouchDeviceDescriptor::Touchpad(desc) => {
                input_device::InputDeviceDescriptor::Touchpad(desc)
            }
        }
    }
}

impl TouchBinding {
    /// Creates a new [`InputDeviceBinding`] from the `device_proxy`.
    ///
    /// The binding will start listening for input reports immediately and send new InputEvents
    /// to the device binding owner over `input_event_sender`.
    ///
    /// # Parameters
    /// - `device_proxy`: The proxy to bind the new [`InputDeviceBinding`] to.
    /// - `device_id`: The id of the connected touch device.
    /// - `input_event_sender`: The channel to send new InputEvents to.
    /// - `device_node`: The inspect node for this device binding
    /// - `metrics_logger`: The metrics logger.
    ///
    /// # Errors
    /// If there was an error binding to the proxy.
    pub async fn new(
        device_proxy: InputDeviceProxy,
        device_id: u32,
        input_event_sender: UnboundedSender<input_device::InputEvent>,
        device_node: fuchsia_inspect::Node,
        metrics_logger: metrics::MetricsLogger,
    ) -> Result<Self, Error> {
        let (device_binding, mut inspect_status) =
            Self::bind_device(device_proxy.clone(), device_id, input_event_sender, device_node)
                .await?;
        device_binding
            .set_touchpad_mode(true)
            .await
            .with_context(|| format!("enabling touchpad mode for device {}", device_id))?;
        inspect_status.health_node.set_ok();
        input_device::initialize_report_stream(
            device_proxy,
            device_binding.get_device_descriptor(),
            device_binding.input_event_sender(),
            inspect_status,
            metrics_logger,
            Self::process_reports,
        );

        Ok(device_binding)
    }

    /// Binds the provided input device to a new instance of `Self`.
    ///
    /// # Parameters
    /// - `device`: The device to use to initialize the binding.
    /// - `device_id`: The id of the connected touch device.
    /// - `input_event_sender`: The channel to send new InputEvents to.
    /// - `device_node`: The inspect node for this device binding
    ///
    /// # Errors
    /// If the device descriptor could not be retrieved, or the descriptor could not be parsed
    /// correctly.
    async fn bind_device(
        device_proxy: InputDeviceProxy,
        device_id: u32,
        input_event_sender: UnboundedSender<input_device::InputEvent>,
        device_node: fuchsia_inspect::Node,
    ) -> Result<(Self, InputDeviceStatus), Error> {
        let mut input_device_status = InputDeviceStatus::new(device_node);
        let device_descriptor: fidl_input_report::DeviceDescriptor = match device_proxy
            .get_descriptor()
            .await
        {
            Ok(descriptor) => descriptor,
            Err(_) => {
                input_device_status.health_node.set_unhealthy("Could not get device descriptor.");
                return Err(format_err!("Could not get descriptor for device_id: {}", device_id));
            }
        };

        let touch_device_type = get_device_type(&device_proxy).await;

        match device_descriptor.touch {
            Some(fidl_fuchsia_input_report::TouchDescriptor {
                input:
                    Some(fidl_fuchsia_input_report::TouchInputDescriptor {
                        contacts: Some(contact_descriptors),
                        max_contacts: _,
                        touch_type: _,
                        buttons: _,
                        ..
                    }),
                ..
            }) => Ok((
                TouchBinding {
                    event_sender: input_event_sender,
                    device_descriptor: match touch_device_type {
                        TouchDeviceType::TouchScreen => {
                            TouchDeviceDescriptor::TouchScreen(TouchScreenDeviceDescriptor {
                                device_id,
                                contacts: contact_descriptors
                                    .iter()
                                    .map(TouchBinding::parse_contact_descriptor)
                                    .filter_map(Result::ok)
                                    .collect(),
                            })
                        }
                        TouchDeviceType::WindowsPrecisionTouchpad => {
                            TouchDeviceDescriptor::Touchpad(TouchpadDeviceDescriptor {
                                device_id,
                                contacts: contact_descriptors
                                    .iter()
                                    .map(TouchBinding::parse_contact_descriptor)
                                    .filter_map(Result::ok)
                                    .collect(),
                            })
                        }
                    },
                    touch_device_type,
                    device_proxy,
                },
                input_device_status,
            )),
            descriptor => {
                input_device_status
                    .health_node
                    .set_unhealthy("Touch Device Descriptor failed to parse.");
                Err(format_err!("Touch Descriptor failed to parse: \n {:?}", descriptor))
            }
        }
    }

    async fn set_touchpad_mode(&self, enable: bool) -> Result<(), Error> {
        match self.touch_device_type {
            TouchDeviceType::TouchScreen => Ok(()),
            TouchDeviceType::WindowsPrecisionTouchpad => {
                // `get_feature_report` to only modify the input_mode and
                // keep other feature as is.
                let mut report = match self.device_proxy.get_feature_report().await? {
                    Ok(report) => report,
                    Err(e) => return Err(format_err!("get_feature_report failed: {}", e)),
                };
                let mut touch =
                    report.touch.unwrap_or(fidl_input_report::TouchFeatureReport::default());
                touch.input_mode = match enable {
                            true => Some(fidl_input_report::TouchConfigurationInputMode::WindowsPrecisionTouchpadCollection),
                            false => Some(fidl_input_report::TouchConfigurationInputMode::MouseCollection),
                        };
                report.touch = Some(touch);
                match self.device_proxy.set_feature_report(&report).await? {
                    Ok(()) => {
                        // TODO(https://fxbug.dev/105092): Remove log message.
                        tracing::info!("touchpad: set touchpad_enabled to {}", enable);
                        Ok(())
                    }
                    Err(e) => Err(format_err!("set_feature_report failed: {}", e)),
                }
            }
        }
    }

    /// Parses an [`InputReport`] into one or more [`InputEvent`]s.
    ///
    /// The [`InputEvent`]s are sent to the device binding owner via [`input_event_sender`].
    ///
    /// # Parameters
    /// - `report`: The incoming [`InputReport`].
    /// - `previous_report`: The previous [`InputReport`] seen for the same device. This can be
    ///                    used to determine, for example, which keys are no longer present in
    ///                    a keyboard report to generate key released events. If `None`, no
    ///                    previous report was found.
    /// - `device_descriptor`: The descriptor for the input device generating the input reports.
    /// - `input_event_sender`: The sender for the device binding's input event stream.
    ///
    /// # Returns
    /// An [`InputReport`] which will be passed to the next call to [`process_reports`], as
    /// [`previous_report`]. If `None`, the next call's [`previous_report`] will be `None`.
    /// A [`UnboundedReceiver<InputEvent>`] which will poll asynchronously generated events to be
    /// recorded by `inspect_status` in `input_device::initialize_report_stream()`. If device
    /// binding does not generate InputEvents asynchronously, this will be `None`.
    fn process_reports(
        report: InputReport,
        previous_report: Option<InputReport>,
        device_descriptor: &input_device::InputDeviceDescriptor,
        input_event_sender: &mut UnboundedSender<InputEvent>,
        inspect_status: &InputDeviceStatus,
        metrics_logger: &metrics::MetricsLogger,
    ) -> (Option<InputReport>, Option<UnboundedReceiver<InputEvent>>) {
        inspect_status.count_received_report(&report);
        match device_descriptor {
            input_device::InputDeviceDescriptor::TouchScreen(_) => process_touch_screen_reports(
                report,
                previous_report,
                device_descriptor,
                input_event_sender,
                inspect_status,
                metrics_logger,
            ),
            input_device::InputDeviceDescriptor::Touchpad(_) => process_touchpad_reports(
                report,
                device_descriptor,
                input_event_sender,
                inspect_status,
                metrics_logger,
            ),
            _ => (None, None),
        }
    }

    /// Parses a fidl_input_report contact descriptor into a [`ContactDeviceDescriptor`]
    ///
    /// # Parameters
    /// - `contact_device_descriptor`: The contact descriptor to parse.
    ///
    /// # Errors
    /// If the contact description fails to parse because required fields aren't present.
    fn parse_contact_descriptor(
        contact_device_descriptor: &fidl_input_report::ContactInputDescriptor,
    ) -> Result<ContactDeviceDescriptor, Error> {
        match contact_device_descriptor {
            fidl_input_report::ContactInputDescriptor {
                position_x: Some(x_axis),
                position_y: Some(y_axis),
                pressure: pressure_axis,
                contact_width: width_axis,
                contact_height: height_axis,
                ..
            } => Ok(ContactDeviceDescriptor {
                x_range: x_axis.range,
                y_range: y_axis.range,
                x_unit: x_axis.unit,
                y_unit: y_axis.unit,
                pressure_range: pressure_axis.map(|axis| axis.range),
                width_range: width_axis.map(|axis| axis.range),
                height_range: height_axis.map(|axis| axis.range),
            }),
            descriptor => {
                Err(format_err!("Touch Contact Descriptor failed to parse: \n {:?}", descriptor))
            }
        }
    }
}

fn process_touch_screen_reports(
    report: InputReport,
    previous_report: Option<InputReport>,
    device_descriptor: &input_device::InputDeviceDescriptor,
    input_event_sender: &mut UnboundedSender<InputEvent>,
    inspect_status: &InputDeviceStatus,
    metrics_logger: &metrics::MetricsLogger,
) -> (Option<InputReport>, Option<UnboundedReceiver<InputEvent>>) {
    fuchsia_trace::duration!("input", "touch-binding-process-report");
    fuchsia_trace::flow_end!("input", "input_report", report.trace_id.unwrap_or(0).into());

    // Input devices can have multiple types so ensure `report` is a TouchInputReport.
    let touch_report: &fidl_fuchsia_input_report::TouchInputReport = match &report.touch {
        Some(touch) => touch,
        None => {
            inspect_status.count_filtered_report(&report);
            return (previous_report, None);
        }
    };

    let previous_contacts: HashMap<u32, TouchContact> = previous_report
        .as_ref()
        .and_then(|unwrapped_report| unwrapped_report.touch.as_ref())
        .map(touch_contacts_from_touch_report)
        .unwrap_or_default();
    let current_contacts: HashMap<u32, TouchContact> =
        touch_contacts_from_touch_report(touch_report);

    // Don't send an event if there are no new contacts.
    if previous_contacts.is_empty() && current_contacts.is_empty() {
        inspect_status.count_filtered_report(&report);
        return (Some(report), None);
    }

    // Contacts which exist only in current.
    let added_contacts: Vec<TouchContact> = Vec::from_iter(
        current_contacts
            .values()
            .cloned()
            .filter(|contact| !previous_contacts.contains_key(&contact.id)),
    );
    // Contacts which exist in both previous and current.
    let moved_contacts: Vec<TouchContact> = Vec::from_iter(
        current_contacts
            .values()
            .cloned()
            .filter(|contact| previous_contacts.contains_key(&contact.id)),
    );
    // Contacts which exist only in previous.
    let removed_contacts: Vec<TouchContact> = Vec::from_iter(
        previous_contacts
            .values()
            .cloned()
            .filter(|contact| !current_contacts.contains_key(&contact.id)),
    );

    let trace_id = fuchsia_trace::Id::new();
    fuchsia_trace::flow_begin!("input", "report-to-event", trace_id);
    send_touch_screen_event(
        hashmap! {
            fidl_ui_input::PointerEventPhase::Add => added_contacts.clone(),
            fidl_ui_input::PointerEventPhase::Down => added_contacts.clone(),
            fidl_ui_input::PointerEventPhase::Move => moved_contacts.clone(),
            fidl_ui_input::PointerEventPhase::Up => removed_contacts.clone(),
            fidl_ui_input::PointerEventPhase::Remove => removed_contacts.clone(),
        },
        hashmap! {
            pointerinjector::EventPhase::Add => added_contacts,
            pointerinjector::EventPhase::Change => moved_contacts,
            pointerinjector::EventPhase::Remove => removed_contacts,
        },
        device_descriptor,
        input_event_sender,
        trace_id,
        inspect_status,
        metrics_logger,
    );

    (Some(report), None)
}

fn process_touchpad_reports(
    report: InputReport,
    device_descriptor: &input_device::InputDeviceDescriptor,
    input_event_sender: &mut UnboundedSender<InputEvent>,
    inspect_status: &InputDeviceStatus,
    metrics_logger: &metrics::MetricsLogger,
) -> (Option<InputReport>, Option<UnboundedReceiver<InputEvent>>) {
    fuchsia_trace::duration!("input", "touch-binding-process-report");
    fuchsia_trace::flow_end!("input", "input_report", report.trace_id.unwrap_or(0).into());

    // Input devices can have multiple types so ensure `report` is a TouchInputReport.
    let touch_report: &fidl_fuchsia_input_report::TouchInputReport = match &report.touch {
        Some(touch) => touch,
        None => {
            inspect_status.count_filtered_report(&report);
            return (None, None);
        }
    };

    let current_contacts: Vec<TouchContact> = touch_report
        .contacts
        .as_ref()
        .and_then(|unwrapped_contacts| {
            // Once the contacts are found, convert them into `TouchContact`s.
            Some(unwrapped_contacts.iter().map(TouchContact::from).collect())
        })
        .unwrap_or_default();

    let buttons: HashSet<mouse_binding::MouseButton> = match &touch_report.pressed_buttons {
        Some(buttons) => HashSet::from_iter(buttons.iter().cloned()),
        None => HashSet::new(),
    };

    let trace_id = fuchsia_trace::Id::new();
    fuchsia_trace::flow_begin!("input", "report-to-event", trace_id);
    send_touchpad_event(
        current_contacts,
        buttons,
        device_descriptor,
        input_event_sender,
        trace_id,
        inspect_status,
        metrics_logger,
    );

    (Some(report), None)
}

fn touch_contacts_from_touch_report(
    touch_report: &fidl_fuchsia_input_report::TouchInputReport,
) -> HashMap<u32, TouchContact> {
    // First unwrap all the optionals in the input report to get to the contacts.
    let contacts: Vec<TouchContact> = touch_report
        .contacts
        .as_ref()
        .and_then(|unwrapped_contacts| {
            // Once the contacts are found, convert them into `TouchContact`s.
            Some(unwrapped_contacts.iter().map(TouchContact::from).collect())
        })
        .unwrap_or_default();

    contacts.into_iter().map(|contact| (contact.id, contact)).collect()
}

/// Sends a TouchScreenEvent over `input_event_sender`.
///
/// # Parameters
/// - `contacts`: The contact points relevant to the new TouchScreenEvent.
/// - `injector_contacts`: The contact points relevant to the new TouchScreenEvent, used to send
///                        pointer events into Scenic.
/// - `device_descriptor`: The descriptor for the input device generating the input reports.
/// - `input_event_sender`: The sender for the device binding's input event stream.
fn send_touch_screen_event(
    contacts: HashMap<fidl_ui_input::PointerEventPhase, Vec<TouchContact>>,
    injector_contacts: HashMap<pointerinjector::EventPhase, Vec<TouchContact>>,
    device_descriptor: &input_device::InputDeviceDescriptor,
    input_event_sender: &mut UnboundedSender<input_device::InputEvent>,
    trace_id: fuchsia_trace::Id,
    inspect_status: &InputDeviceStatus,
    metrics_logger: &metrics::MetricsLogger,
) {
    let event = input_device::InputEvent {
        device_event: input_device::InputDeviceEvent::TouchScreen(TouchScreenEvent {
            contacts,
            injector_contacts,
        }),
        device_descriptor: device_descriptor.clone(),
        event_time: zx::Time::get_monotonic(),
        handled: Handled::No,
        trace_id: Some(trace_id),
    };

    match input_event_sender.unbounded_send(event.clone()) {
        Err(e) => {
            metrics_logger.log_error(
                InputPipelineErrorMetricDimensionEvent::TouchFailedToSendTouchScreenEvent,
                std::format!("Failed to send TouchScreenEvent with error: {:?}", e),
            );
        }
        _ => inspect_status.count_generated_event(event),
    }
}

/// Sends a TouchpadEvent over `input_event_sender`.
///
/// # Parameters
/// - `injector_contacts`: The contact points relevant to the new TouchpadEvent.
/// - `pressed_buttons`: The pressing button of the new TouchpadEvent.
/// - `device_descriptor`: The descriptor for the input device generating the input reports.
/// - `input_event_sender`: The sender for the device binding's input event stream.
fn send_touchpad_event(
    injector_contacts: Vec<TouchContact>,
    pressed_buttons: HashSet<mouse_binding::MouseButton>,
    device_descriptor: &input_device::InputDeviceDescriptor,
    input_event_sender: &mut UnboundedSender<input_device::InputEvent>,
    trace_id: fuchsia_trace::Id,
    inspect_status: &InputDeviceStatus,
    metrics_logger: &metrics::MetricsLogger,
) {
    let event = input_device::InputEvent {
        device_event: input_device::InputDeviceEvent::Touchpad(TouchpadEvent {
            injector_contacts,
            pressed_buttons,
        }),
        device_descriptor: device_descriptor.clone(),
        event_time: zx::Time::get_monotonic(),
        handled: Handled::No,
        trace_id: Some(trace_id),
    };

    match input_event_sender.unbounded_send(event.clone()) {
        Err(e) => {
            metrics_logger.log_error(
                InputPipelineErrorMetricDimensionEvent::TouchFailedToSendTouchpadEvent,
                std::format!("Failed to send TouchpadEvent with error: {:?}", e),
            );
        }
        _ => inspect_status.count_generated_event(event),
    }
}

/// [`get_device_type`] check if the touch device is a touchscreen or Windows Precision Touchpad.
///
/// Windows Precision Touchpad reports `MouseCollection` or `WindowsPrecisionTouchpadCollection`
/// in `TouchFeatureReport`. Fallback all error responses on `get_feature_report` to TouchScreen
/// because some touch screen does not report this method.
async fn get_device_type(input_device: &fidl_input_report::InputDeviceProxy) -> TouchDeviceType {
    match input_device.get_feature_report().await {
        Ok(Ok(fidl_input_report::FeatureReport {
            touch:
                Some(fidl_input_report::TouchFeatureReport {
                    input_mode:
                        Some(
                            fidl_input_report::TouchConfigurationInputMode::MouseCollection
                            | fidl_input_report::TouchConfigurationInputMode::WindowsPrecisionTouchpadCollection,
                        ),
                    ..
                }),
            ..
        })) => TouchDeviceType::WindowsPrecisionTouchpad,
        _ => TouchDeviceType::TouchScreen,
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::testing_utilities::{
            self, create_touch_contact, create_touch_input_report, create_touch_screen_event,
            create_touchpad_event,
        },
        crate::utils::Position,
        assert_matches::assert_matches,
        fidl::endpoints::spawn_stream_handler,
        fuchsia_async as fasync,
        fuchsia_inspect::AnyProperty,
        futures::StreamExt,
        pretty_assertions::assert_eq,
        test_case::test_case,
    };

    #[fasync::run_singlethreaded(test)]
    async fn process_empty_reports() {
        let previous_report_time = fuchsia_zircon::Time::get_monotonic().into_nanos();
        let previous_report = create_touch_input_report(
            vec![],
            /* pressed_buttons= */ None,
            previous_report_time,
        );
        let report_time = fuchsia_zircon::Time::get_monotonic().into_nanos();
        let report =
            create_touch_input_report(vec![], /* pressed_buttons= */ None, report_time);

        let descriptor =
            input_device::InputDeviceDescriptor::TouchScreen(TouchScreenDeviceDescriptor {
                device_id: 1,
                contacts: vec![],
            });
        let (mut event_sender, mut event_receiver) = futures::channel::mpsc::unbounded();

        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("TestDevice_Touch");
        let mut inspect_status = InputDeviceStatus::new(test_node);
        inspect_status.health_node.set_ok();

        let (returned_report, _) = TouchBinding::process_reports(
            report,
            Some(previous_report),
            &descriptor,
            &mut event_sender,
            &inspect_status,
            &metrics::MetricsLogger::default(),
        );
        assert!(returned_report.is_some());
        assert_eq!(returned_report.unwrap().event_time, Some(report_time));

        // Assert there are no pending events on the receiver.
        let event = event_receiver.try_next();
        assert!(event.is_err());

        fuchsia_inspect::assert_data_tree!(inspector, root: {
            "TestDevice_Touch": contains {
                reports_received_count: 1u64,
                reports_filtered_count: 1u64,
                events_generated: 0u64,
                last_received_timestamp_ns: report_time as u64,
                last_generated_timestamp_ns: 0u64,
                "fuchsia.inspect.Health": {
                    status: "OK",
                    // Timestamp value is unpredictable and not relevant in this context,
                    // so we only assert that the property is present.
                    start_timestamp_nanos: AnyProperty
                },
            }
        });
    }

    // Tests that a input report with a new contact generates an event with an add and a down.
    #[fasync::run_singlethreaded(test)]
    async fn add_and_down() {
        const TOUCH_ID: u32 = 2;

        let descriptor =
            input_device::InputDeviceDescriptor::TouchScreen(TouchScreenDeviceDescriptor {
                device_id: 1,
                contacts: vec![],
            });
        let (event_time_i64, event_time_u64) = testing_utilities::event_times();

        let contact = fidl_fuchsia_input_report::ContactInputReport {
            contact_id: Some(TOUCH_ID),
            position_x: Some(0),
            position_y: Some(0),
            pressure: None,
            contact_width: None,
            contact_height: None,
            ..Default::default()
        };
        let reports = vec![create_touch_input_report(
            vec![contact],
            /* pressed_buttons= */ None,
            event_time_i64,
        )];

        let expected_events = vec![create_touch_screen_event(
            hashmap! {
                fidl_ui_input::PointerEventPhase::Add
                    => vec![create_touch_contact(TOUCH_ID, Position { x: 0.0, y: 0.0 })],
                fidl_ui_input::PointerEventPhase::Down
                    => vec![create_touch_contact(TOUCH_ID, Position { x: 0.0, y: 0.0 })],
            },
            event_time_u64,
            &descriptor,
        )];

        assert_input_report_sequence_generates_events!(
            input_reports: reports,
            expected_events: expected_events,
            device_descriptor: descriptor,
            device_type: TouchBinding,
        );
    }

    // Tests that up and remove events are sent when a touch is released.
    #[fasync::run_singlethreaded(test)]
    async fn up_and_remove() {
        const TOUCH_ID: u32 = 2;

        let descriptor =
            input_device::InputDeviceDescriptor::TouchScreen(TouchScreenDeviceDescriptor {
                device_id: 1,
                contacts: vec![],
            });
        let (event_time_i64, event_time_u64) = testing_utilities::event_times();

        let contact = fidl_fuchsia_input_report::ContactInputReport {
            contact_id: Some(TOUCH_ID),
            position_x: Some(0),
            position_y: Some(0),
            pressure: None,
            contact_width: None,
            contact_height: None,
            ..Default::default()
        };
        let reports = vec![
            create_touch_input_report(
                vec![contact],
                /* pressed_buttons= */ None,
                event_time_i64,
            ),
            create_touch_input_report(vec![], /* pressed_buttons= */ None, event_time_i64),
        ];

        let expected_events = vec![
            create_touch_screen_event(
                hashmap! {
                    fidl_ui_input::PointerEventPhase::Add
                        => vec![create_touch_contact(TOUCH_ID, Position { x: 0.0, y: 0.0 })],
                    fidl_ui_input::PointerEventPhase::Down
                        => vec![create_touch_contact(TOUCH_ID, Position { x: 0.0, y: 0.0 })],
                },
                event_time_u64,
                &descriptor,
            ),
            create_touch_screen_event(
                hashmap! {
                    fidl_ui_input::PointerEventPhase::Up
                        => vec![create_touch_contact(TOUCH_ID, Position { x: 0.0, y: 0.0 })],
                    fidl_ui_input::PointerEventPhase::Remove
                        => vec![create_touch_contact(TOUCH_ID, Position { x: 0.0, y: 0.0 })],
                },
                event_time_u64,
                &descriptor,
            ),
        ];

        assert_input_report_sequence_generates_events!(
            input_reports: reports,
            expected_events: expected_events,
            device_descriptor: descriptor,
            device_type: TouchBinding,
        );
    }

    // Tests that a move generates the correct event.
    #[fasync::run_singlethreaded(test)]
    async fn add_down_move() {
        const TOUCH_ID: u32 = 2;
        let first = Position { x: 10.0, y: 30.0 };
        let second = Position { x: first.x * 2.0, y: first.y * 2.0 };

        let descriptor =
            input_device::InputDeviceDescriptor::TouchScreen(TouchScreenDeviceDescriptor {
                device_id: 1,
                contacts: vec![],
            });
        let (event_time_i64, event_time_u64) = testing_utilities::event_times();

        let first_contact = fidl_fuchsia_input_report::ContactInputReport {
            contact_id: Some(TOUCH_ID),
            position_x: Some(first.x as i64),
            position_y: Some(first.y as i64),
            pressure: None,
            contact_width: None,
            contact_height: None,
            ..Default::default()
        };
        let second_contact = fidl_fuchsia_input_report::ContactInputReport {
            contact_id: Some(TOUCH_ID),
            position_x: Some(first.x as i64 * 2),
            position_y: Some(first.y as i64 * 2),
            pressure: None,
            contact_width: None,
            contact_height: None,
            ..Default::default()
        };

        let reports = vec![
            create_touch_input_report(
                vec![first_contact],
                /* pressed_buttons= */ None,
                event_time_i64,
            ),
            create_touch_input_report(
                vec![second_contact],
                /* pressed_buttons= */ None,
                event_time_i64,
            ),
        ];

        let expected_events = vec![
            create_touch_screen_event(
                hashmap! {
                    fidl_ui_input::PointerEventPhase::Add
                        => vec![create_touch_contact(TOUCH_ID, first)],
                    fidl_ui_input::PointerEventPhase::Down
                        => vec![create_touch_contact(TOUCH_ID, first)],
                },
                event_time_u64,
                &descriptor,
            ),
            create_touch_screen_event(
                hashmap! {
                    fidl_ui_input::PointerEventPhase::Move
                        => vec![create_touch_contact(TOUCH_ID, second)],
                },
                event_time_u64,
                &descriptor,
            ),
        ];

        assert_input_report_sequence_generates_events!(
            input_reports: reports,
            expected_events: expected_events,
            device_descriptor: descriptor,
            device_type: TouchBinding,
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn sent_event_has_trace_id() {
        let previous_report_time = fuchsia_zircon::Time::get_monotonic().into_nanos();
        let previous_report = create_touch_input_report(
            vec![],
            /* pressed_buttons= */ None,
            previous_report_time,
        );

        let report_time = fuchsia_zircon::Time::get_monotonic().into_nanos();
        let contact = fidl_fuchsia_input_report::ContactInputReport {
            contact_id: Some(222),
            position_x: Some(333),
            position_y: Some(444),
            ..Default::default()
        };
        let report =
            create_touch_input_report(vec![contact], /* pressed_buttons= */ None, report_time);

        let descriptor =
            input_device::InputDeviceDescriptor::TouchScreen(TouchScreenDeviceDescriptor {
                device_id: 1,
                contacts: vec![],
            });
        let (mut event_sender, mut event_receiver) = futures::channel::mpsc::unbounded();

        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("TestDevice_Touch");
        let mut inspect_status = InputDeviceStatus::new(test_node);
        inspect_status.health_node.set_ok();

        let _ = TouchBinding::process_reports(
            report,
            Some(previous_report),
            &descriptor,
            &mut event_sender,
            &inspect_status,
            &metrics::MetricsLogger::default(),
        );
        assert_matches!(event_receiver.try_next(), Ok(Some(InputEvent { trace_id: Some(_), .. })));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn enables_touchpad_mode_automatically() {
        let (set_feature_report_sender, set_feature_report_receiver) =
            futures::channel::mpsc::unbounded();
        let input_device_proxy = spawn_stream_handler(move |input_device_request| {
            let set_feature_report_sender = set_feature_report_sender.clone();
            async move {
                match input_device_request {
                    fidl_input_report::InputDeviceRequest::GetDescriptor { responder } => {
                        let _ = responder.send(&get_touchpad_device_descriptor(
                            true, /* has_mouse_descriptor */
                        ));
                    }
                    fidl_input_report::InputDeviceRequest::GetFeatureReport { responder } => {
                        let _ = responder.send(Ok(&fidl_input_report::FeatureReport {
                            touch: Some(fidl_input_report::TouchFeatureReport {
                                input_mode: Some(
                                    fidl_input_report::TouchConfigurationInputMode::MouseCollection,
                                ),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }));
                    }
                    fidl_input_report::InputDeviceRequest::SetFeatureReport {
                        responder,
                        report,
                    } => {
                        match set_feature_report_sender.unbounded_send(report) {
                            Ok(_) => {
                                let _ = responder.send(Ok(()));
                            }
                            Err(e) => {
                                panic!("try_send set_feature_report_request failed: {}", e);
                            }
                        };
                    }
                    fidl_input_report::InputDeviceRequest::GetInputReportsReader { .. } => {
                        // Do not panic as `initialize_report_stream()` will call this protocol.
                    }
                    r => panic!("unsupported request {:?}", r),
                }
            }
        })
        .unwrap();

        let (device_event_sender, _) = futures::channel::mpsc::unbounded();

        // Create a test inspect node as required by TouchBinding::new()
        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("test_node");

        // Create a `TouchBinding` to exercise its call to `SetFeatureReport`. But drop
        // the binding immediately, so that `set_feature_report_receiver.collect()`
        // does not hang.
        TouchBinding::new(
            input_device_proxy,
            0,
            device_event_sender,
            test_node,
            metrics::MetricsLogger::default(),
        )
        .await
        .unwrap();
        assert_matches!(
            set_feature_report_receiver.collect::<Vec<_>>().await.as_slice(),
            [fidl_input_report::FeatureReport {
                touch: Some(fidl_input_report::TouchFeatureReport {
                    input_mode: Some(
                        fidl_input_report::TouchConfigurationInputMode::WindowsPrecisionTouchpadCollection
                    ),
                    ..
                }),
                ..
            }]
        );
    }

    #[test_case(true, None, TouchDeviceType::TouchScreen; "touch screen")]
    #[test_case(false, None, TouchDeviceType::TouchScreen; "no mouse descriptor, no touch_input_mode")]
    #[test_case(true, Some(fidl_input_report::TouchConfigurationInputMode::MouseCollection), TouchDeviceType::WindowsPrecisionTouchpad; "touchpad in mouse mode")]
    #[test_case(true, Some(fidl_input_report::TouchConfigurationInputMode::WindowsPrecisionTouchpadCollection), TouchDeviceType::WindowsPrecisionTouchpad; "touchpad in touchpad mode")]
    #[fuchsia::test(allow_stalls = false)]
    async fn identifies_correct_touch_device_type(
        has_mouse_descriptor: bool,
        touch_input_mode: Option<fidl_input_report::TouchConfigurationInputMode>,
        expect_touch_device_type: TouchDeviceType,
    ) {
        let input_device_proxy = spawn_stream_handler(move |input_device_request| async move {
            match input_device_request {
                fidl_input_report::InputDeviceRequest::GetDescriptor { responder } => {
                    let _ = responder.send(&get_touchpad_device_descriptor(has_mouse_descriptor));
                }
                fidl_input_report::InputDeviceRequest::GetFeatureReport { responder } => {
                    let _ = responder.send(Ok(&fidl_input_report::FeatureReport {
                        touch: Some(fidl_input_report::TouchFeatureReport {
                            input_mode: touch_input_mode,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }));
                }
                fidl_input_report::InputDeviceRequest::SetFeatureReport { responder, .. } => {
                    let _ = responder.send(Ok(()));
                }
                r => panic!("unsupported request {:?}", r),
            }
        })
        .unwrap();

        let (device_event_sender, _) = futures::channel::mpsc::unbounded();

        // Create a test inspect node as required by TouchBinding::new()
        let inspector = fuchsia_inspect::Inspector::default();
        let test_node = inspector.root().create_child("test_node");

        let binding = TouchBinding::new(
            input_device_proxy,
            0,
            device_event_sender,
            test_node,
            metrics::MetricsLogger::default(),
        )
        .await
        .unwrap();
        pretty_assertions::assert_eq!(binding.touch_device_type, expect_touch_device_type);
    }

    /// Returns an |fidl_fuchsia_input_report::DeviceDescriptor| for
    /// touchpad related tests.
    fn get_touchpad_device_descriptor(
        has_mouse_descriptor: bool,
    ) -> fidl_fuchsia_input_report::DeviceDescriptor {
        fidl_input_report::DeviceDescriptor {
            mouse: match has_mouse_descriptor {
                true => Some(fidl_input_report::MouseDescriptor::default()),
                false => None,
            },
            touch: Some(fidl_input_report::TouchDescriptor {
                input: Some(fidl_input_report::TouchInputDescriptor {
                    contacts: Some(vec![fidl_input_report::ContactInputDescriptor {
                        position_x: Some(fidl_input_report::Axis {
                            range: fidl_input_report::Range { min: 1, max: 2 },
                            unit: fidl_input_report::Unit {
                                type_: fidl_input_report::UnitType::None,
                                exponent: 0,
                            },
                        }),
                        position_y: Some(fidl_input_report::Axis {
                            range: fidl_input_report::Range { min: 2, max: 3 },
                            unit: fidl_input_report::Unit {
                                type_: fidl_input_report::UnitType::Other,
                                exponent: 100000,
                            },
                        }),
                        pressure: Some(fidl_input_report::Axis {
                            range: fidl_input_report::Range { min: 3, max: 4 },
                            unit: fidl_input_report::Unit {
                                type_: fidl_input_report::UnitType::Grams,
                                exponent: -991,
                            },
                        }),
                        contact_width: Some(fidl_input_report::Axis {
                            range: fidl_input_report::Range { min: 5, max: 6 },
                            unit: fidl_input_report::Unit {
                                type_: fidl_input_report::UnitType::EnglishAngularVelocity,
                                exponent: 123,
                            },
                        }),
                        contact_height: Some(fidl_input_report::Axis {
                            range: fidl_input_report::Range { min: 7, max: 8 },
                            unit: fidl_input_report::Unit {
                                type_: fidl_input_report::UnitType::Pascals,
                                exponent: 100,
                            },
                        }),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[fasync::run_singlethreaded(test)]
    async fn send_touchpad_event_button() {
        const TOUCH_ID: u32 = 1;
        const PRIMARY_BUTTON: u8 = 1;

        let descriptor = input_device::InputDeviceDescriptor::Touchpad(TouchpadDeviceDescriptor {
            device_id: 1,
            contacts: vec![],
        });
        let (event_time_i64, event_time_u64) = testing_utilities::event_times();

        let contact = fidl_fuchsia_input_report::ContactInputReport {
            contact_id: Some(TOUCH_ID),
            position_x: Some(0),
            position_y: Some(0),
            pressure: None,
            contact_width: None,
            contact_height: None,
            ..Default::default()
        };
        let reports = vec![create_touch_input_report(
            vec![contact],
            Some(vec![PRIMARY_BUTTON]),
            event_time_i64,
        )];

        let expected_events = vec![create_touchpad_event(
            vec![create_touch_contact(TOUCH_ID, Position { x: 0.0, y: 0.0 })],
            vec![PRIMARY_BUTTON].into_iter().collect(),
            event_time_u64,
            &descriptor,
        )];

        assert_input_report_sequence_generates_events!(
            input_reports: reports,
            expected_events: expected_events,
            device_descriptor: descriptor,
            device_type: TouchBinding,
        );
    }

    #[fasync::run_singlethreaded(test)]
    async fn send_touchpad_event_2_fingers_down_up() {
        const TOUCH_ID_1: u32 = 1;
        const TOUCH_ID_2: u32 = 2;

        let descriptor = input_device::InputDeviceDescriptor::Touchpad(TouchpadDeviceDescriptor {
            device_id: 1,
            contacts: vec![],
        });
        let (event_time_i64, event_time_u64) = testing_utilities::event_times();

        let contact1 = fidl_fuchsia_input_report::ContactInputReport {
            contact_id: Some(TOUCH_ID_1),
            position_x: Some(0),
            position_y: Some(0),
            pressure: None,
            contact_width: None,
            contact_height: None,
            ..Default::default()
        };
        let contact2 = fidl_fuchsia_input_report::ContactInputReport {
            contact_id: Some(TOUCH_ID_2),
            position_x: Some(10),
            position_y: Some(10),
            pressure: None,
            contact_width: None,
            contact_height: None,
            ..Default::default()
        };
        let reports = vec![
            create_touch_input_report(
                vec![contact1, contact2],
                /* pressed_buttons= */ None,
                event_time_i64,
            ),
            create_touch_input_report(vec![], /* pressed_buttons= */ None, event_time_i64),
        ];

        let expected_events = vec![
            create_touchpad_event(
                vec![
                    create_touch_contact(TOUCH_ID_1, Position { x: 0.0, y: 0.0 }),
                    create_touch_contact(TOUCH_ID_2, Position { x: 10.0, y: 10.0 }),
                ],
                HashSet::new(),
                event_time_u64,
                &descriptor,
            ),
            create_touchpad_event(vec![], HashSet::new(), event_time_u64, &descriptor),
        ];

        assert_input_report_sequence_generates_events!(
            input_reports: reports,
            expected_events: expected_events,
            device_descriptor: descriptor,
            device_type: TouchBinding,
        );
    }

    #[test_case(Position{x: 0.0, y: 0.0}, Position{x: 5.0, y: 5.0}; "down move")]
    #[test_case(Position{x: 0.0, y: 0.0}, Position{x: 0.0, y: 0.0}; "down hold")]
    #[fasync::run_singlethreaded(test)]
    async fn send_touchpad_event_1_finger(p0: Position, p1: Position) {
        const TOUCH_ID: u32 = 1;

        let descriptor = input_device::InputDeviceDescriptor::Touchpad(TouchpadDeviceDescriptor {
            device_id: 1,
            contacts: vec![],
        });
        let (event_time_i64, event_time_u64) = testing_utilities::event_times();

        let contact1 = fidl_fuchsia_input_report::ContactInputReport {
            contact_id: Some(TOUCH_ID),
            position_x: Some(p0.x as i64),
            position_y: Some(p0.y as i64),
            pressure: None,
            contact_width: None,
            contact_height: None,
            ..Default::default()
        };
        let contact2 = fidl_fuchsia_input_report::ContactInputReport {
            contact_id: Some(TOUCH_ID),
            position_x: Some(p1.x as i64),
            position_y: Some(p1.y as i64),
            pressure: None,
            contact_width: None,
            contact_height: None,
            ..Default::default()
        };
        let reports = vec![
            create_touch_input_report(
                vec![contact1],
                /* pressed_buttons= */ None,
                event_time_i64,
            ),
            create_touch_input_report(
                vec![contact2],
                /* pressed_buttons= */ None,
                event_time_i64,
            ),
        ];

        let expected_events = vec![
            create_touchpad_event(
                vec![create_touch_contact(TOUCH_ID, p0)],
                HashSet::new(),
                event_time_u64,
                &descriptor,
            ),
            create_touchpad_event(
                vec![create_touch_contact(TOUCH_ID, p1)],
                HashSet::new(),
                event_time_u64,
                &descriptor,
            ),
        ];

        assert_input_report_sequence_generates_events!(
            input_reports: reports,
            expected_events: expected_events,
            device_descriptor: descriptor,
            device_type: TouchBinding,
        );
    }
}
