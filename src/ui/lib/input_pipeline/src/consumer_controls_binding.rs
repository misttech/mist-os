// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::input_device::{self, Handled, InputDeviceBinding, InputDeviceStatus, InputEvent},
    crate::metrics,
    anyhow::{format_err, Error},
    async_trait::async_trait,
    fidl_fuchsia_input_report::{self as fidl_input_report, ConsumerControlButton},
    fidl_fuchsia_input_report::{InputDeviceProxy, InputReport},
    fuchsia_inspect::{health::Reporter, ArrayProperty},
    fuchsia_zircon as zx,
    futures::channel::mpsc::{UnboundedReceiver, UnboundedSender},
    metrics_registry::*,
};

/// A [`ConsumerControlsEvent`] represents an event where one or more consumer control buttons
/// were pressed.
///
/// # Example
/// The following ConsumerControlsEvents represents an event where the volume up button was pressed.
///
/// ```
/// let volume_event = input_device::InputDeviceEvent::ConsumerControls(ConsumerControlsEvent::new(
///     vec![ConsumerControlButton::VOLUME_UP],
/// ));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct ConsumerControlsEvent {
    pub pressed_buttons: Vec<ConsumerControlButton>,
}

impl ConsumerControlsEvent {
    /// Creates a new [`ConsumerControlsEvent`] with the relevant buttons.
    ///
    /// # Parameters
    /// - `pressed_buttons`: The buttons relevant to this event.
    pub fn new(pressed_buttons: Vec<ConsumerControlButton>) -> Self {
        Self { pressed_buttons }
    }

    pub fn record_inspect(&self, node: &fuchsia_inspect::Node) {
        let pressed_buttons_node =
            node.create_string_array("pressed_buttons", self.pressed_buttons.len());
        self.pressed_buttons.iter().enumerate().for_each(|(i, &ref button)| {
            pressed_buttons_node.set(
                i,
                match button {
                    ConsumerControlButton::VolumeUp => "volume_up",
                    ConsumerControlButton::VolumeDown => "volume_down",
                    ConsumerControlButton::Pause => "pause",
                    ConsumerControlButton::FactoryReset => "factory_reset",
                    ConsumerControlButton::MicMute => "mic_mute",
                    ConsumerControlButton::Reboot => "reboot",
                    ConsumerControlButton::CameraDisable => "camera_disable",
                    _ => "unknown",
                },
            );
        });
        node.record(pressed_buttons_node);
    }
}

/// A [`ConsumerControlsBinding`] represents a connection to a consumer controls input device with
/// consumer controls. The buttons supported by this binding is returned by `supported_buttons()`.
///
/// The [`ConsumerControlsBinding`] parses and exposes consumer control descriptor properties
/// for the device it is associated with. It also parses [`InputReport`]s
/// from the device, and sends them to the device binding owner over `event_sender`.
pub struct ConsumerControlsBinding {
    /// The channel to stream InputEvents to.
    event_sender: UnboundedSender<input_device::InputEvent>,

    /// Holds information about this device.
    device_descriptor: ConsumerControlsDeviceDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsumerControlsDeviceDescriptor {
    /// The list of buttons that this device contains.
    pub buttons: Vec<ConsumerControlButton>,
}

#[async_trait]
impl input_device::InputDeviceBinding for ConsumerControlsBinding {
    fn input_event_sender(&self) -> UnboundedSender<input_device::InputEvent> {
        self.event_sender.clone()
    }

    fn get_device_descriptor(&self) -> input_device::InputDeviceDescriptor {
        input_device::InputDeviceDescriptor::ConsumerControls(self.device_descriptor.clone())
    }
}

impl ConsumerControlsBinding {
    /// Creates a new [`InputDeviceBinding`] from the `device_proxy`.
    ///
    /// The binding will start listening for input reports immediately and send new InputEvents
    /// to the device binding owner over `input_event_sender`.
    ///
    /// # Parameters
    /// - `device_proxy`: The proxy to bind the new [`InputDeviceBinding`] to.
    /// - `device_id`: The id of the connected device.
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
            Self::bind_device(&device_proxy, device_id, input_event_sender, device_node).await?;
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
    /// - `device_id`: The id of the connected device.
    /// - `input_event_sender`: The channel to send new InputEvents to.
    /// - `device_node`: The inspect node for this device binding
    ///
    /// # Errors
    /// If the device descriptor could not be retrieved, or the descriptor could
    /// not be parsed correctly.
    async fn bind_device(
        device: &InputDeviceProxy,
        device_id: u32,
        input_event_sender: UnboundedSender<input_device::InputEvent>,
        device_node: fuchsia_inspect::Node,
    ) -> Result<(Self, InputDeviceStatus), Error> {
        let mut input_device_status = InputDeviceStatus::new(device_node);
        let device_descriptor: fidl_input_report::DeviceDescriptor = match device
            .get_descriptor()
            .await
        {
            Ok(descriptor) => descriptor,
            Err(_) => {
                input_device_status.health_node.set_unhealthy("Could not get device descriptor.");
                return Err(format_err!("Could not get descriptor for device_id: {}", device_id));
            }
        };

        let consumer_controls_descriptor = device_descriptor.consumer_control.ok_or_else(|| {
            input_device_status
                .health_node
                .set_unhealthy("DeviceDescriptor does not have a ConsumerControlDescriptor.");
            format_err!("DeviceDescriptor does not have a ConsumerControlDescriptor")
        })?;

        let consumer_controls_input_descriptor =
            consumer_controls_descriptor.input.ok_or_else(|| {
                input_device_status.health_node.set_unhealthy(
                    "ConsumerControlDescriptor does not have a ConsumerControlInputDescriptor.",
                );
                format_err!(
                    "ConsumerControlDescriptor does not have a ConsumerControlInputDescriptor"
                )
            })?;

        let device_descriptor: ConsumerControlsDeviceDescriptor =
            ConsumerControlsDeviceDescriptor {
                buttons: consumer_controls_input_descriptor.buttons.unwrap_or_default(),
            };

        Ok((
            ConsumerControlsBinding { event_sender: input_event_sender, device_descriptor },
            input_device_status,
        ))
    }

    /// Parses an [`InputReport`] into one or more [`InputEvent`]s. Sends the [`InputEvent`]s
    /// to the device binding owner via [`input_event_sender`].
    ///
    /// # Parameters
    /// `report`: The incoming [`InputReport`].
    /// `previous_report`: The previous [`InputReport`] seen for the same device. This can be
    ///                    used to determine, for example, which keys are no longer present in
    ///                    a keyboard report to generate key released events. If `None`, no
    ///                    previous report was found.
    /// `device_descriptor`: The descriptor for the input device generating the input reports.
    /// `input_event_sender`: The sender for the device binding's input event stream.
    /// `metrics_logger`: The metrics logger.
    ///
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
        input_event_sender: &mut UnboundedSender<input_device::InputEvent>,
        inspect_status: &InputDeviceStatus,
        metrics_logger: &metrics::MetricsLogger,
    ) -> (Option<InputReport>, Option<UnboundedReceiver<InputEvent>>) {
        inspect_status.count_received_report(&report);
        // Input devices can have multiple types so ensure `report` is a ConsumerControlInputReport.
        let pressed_buttons: Vec<ConsumerControlButton> = match report.consumer_control {
            Some(ref consumer_control_report) => consumer_control_report
                .pressed_buttons
                .as_ref()
                .map(|buttons| buttons.iter().cloned().collect())
                .unwrap_or_default(),
            None => {
                inspect_status.count_filtered_report(&report);
                return (previous_report, None);
            }
        };

        send_consumer_controls_event(
            pressed_buttons,
            device_descriptor,
            input_event_sender,
            inspect_status,
            metrics_logger,
        );

        (Some(report), None)
    }
}

/// Sends an InputEvent over `sender`.
///
/// # Parameters
/// - `pressed_buttons`: The buttons relevant to the event.
/// - `device_descriptor`: The descriptor for the input device generating the input reports.
/// - `sender`: The stream to send the InputEvent to.
/// - `metrics_logger`: The metrics logger.
fn send_consumer_controls_event(
    pressed_buttons: Vec<ConsumerControlButton>,
    device_descriptor: &input_device::InputDeviceDescriptor,
    sender: &mut UnboundedSender<input_device::InputEvent>,
    inspect_status: &InputDeviceStatus,
    metrics_logger: &metrics::MetricsLogger,
) {
    let event = input_device::InputEvent {
        device_event: input_device::InputDeviceEvent::ConsumerControls(ConsumerControlsEvent::new(
            pressed_buttons,
        )),
        device_descriptor: device_descriptor.clone(),
        event_time: zx::Time::get_monotonic(),
        handled: Handled::No,
        trace_id: None,
    };

    match sender.unbounded_send(event.clone()) {
        Err(e) => metrics_logger.log_error(
            InputPipelineErrorMetricDimensionEvent::ConsumerControlsSendEventFailed,
            std::format!("Failed to send ConsumerControlsEvent with error: {:?}", e),
        ),
        _ => inspect_status.count_generated_event(event),
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, crate::testing_utilities, fuchsia_async as fasync, fuchsia_inspect::AnyProperty,
        futures::StreamExt,
    };

    // Tests that an InputReport containing one consumer control button generates an InputEvent
    // containing the same consumer control button.
    #[fasync::run_singlethreaded(test)]
    async fn volume_up_only() {
        let (event_time_i64, event_time_u64) = testing_utilities::event_times();
        let pressed_buttons = vec![ConsumerControlButton::VolumeUp];
        let first_report = testing_utilities::create_consumer_control_input_report(
            pressed_buttons.clone(),
            event_time_i64,
        );
        let descriptor = testing_utilities::consumer_controls_device_descriptor();

        let input_reports = vec![first_report];
        let expected_events = vec![testing_utilities::create_consumer_controls_event(
            pressed_buttons,
            event_time_u64,
            &descriptor,
        )];

        assert_input_report_sequence_generates_events!(
            input_reports: input_reports,
            expected_events: expected_events,
            device_descriptor: descriptor,
            device_type: ConsumerControlsBinding,
        );
    }

    // Tests that an InputReport containing two consumer control buttons generates an InputEvent
    // containing both consumer control buttons.
    #[fasync::run_singlethreaded(test)]
    async fn volume_up_and_down() {
        let (event_time_i64, event_time_u64) = testing_utilities::event_times();
        let pressed_buttons =
            vec![ConsumerControlButton::VolumeUp, ConsumerControlButton::VolumeDown];
        let first_report = testing_utilities::create_consumer_control_input_report(
            pressed_buttons.clone(),
            event_time_i64,
        );
        let descriptor = testing_utilities::consumer_controls_device_descriptor();

        let input_reports = vec![first_report];
        let expected_events = vec![testing_utilities::create_consumer_controls_event(
            pressed_buttons,
            event_time_u64,
            &descriptor,
        )];

        assert_input_report_sequence_generates_events!(
            input_reports: input_reports,
            expected_events: expected_events,
            device_descriptor: descriptor,
            device_type: ConsumerControlsBinding,
        );
    }

    // Tests that three InputReports containing one consumer control button generates three
    // InputEvents containing the same consumer control button.
    #[fasync::run_singlethreaded(test)]
    async fn sequence_of_buttons() {
        let (event_time_i64, event_time_u64) = testing_utilities::event_times();
        let first_report = testing_utilities::create_consumer_control_input_report(
            vec![ConsumerControlButton::VolumeUp],
            event_time_i64,
        );
        let second_report = testing_utilities::create_consumer_control_input_report(
            vec![ConsumerControlButton::VolumeDown],
            event_time_i64,
        );
        let third_report = testing_utilities::create_consumer_control_input_report(
            vec![ConsumerControlButton::CameraDisable],
            event_time_i64,
        );
        let descriptor = testing_utilities::consumer_controls_device_descriptor();

        let input_reports = vec![first_report, second_report, third_report];
        let expected_events = vec![
            testing_utilities::create_consumer_controls_event(
                vec![ConsumerControlButton::VolumeUp],
                event_time_u64,
                &descriptor,
            ),
            testing_utilities::create_consumer_controls_event(
                vec![ConsumerControlButton::VolumeDown],
                event_time_u64,
                &descriptor,
            ),
            testing_utilities::create_consumer_controls_event(
                vec![ConsumerControlButton::CameraDisable],
                event_time_u64,
                &descriptor,
            ),
        ];

        assert_input_report_sequence_generates_events!(
            input_reports: input_reports,
            expected_events: expected_events,
            device_descriptor: descriptor,
            device_type: ConsumerControlsBinding,
        );
    }
}
