// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::types::Rgbc;
use crate::input_device::{
    self, Handled, InputDeviceBinding, InputDeviceDescriptor, InputDeviceStatus, InputEvent,
};
use anyhow::{format_err, Error};
use async_trait::async_trait;
use fidl_fuchsia_input_report::{InputDeviceProxy, InputReport, SensorDescriptor, SensorType};
use fidl_fuchsia_ui_input_config::FeaturesRequest as InputConfigFeaturesRequest;
use fuchsia_inspect::health::Reporter;
use fuchsia_zircon as zx;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};

#[derive(Clone, Debug)]
pub struct LightSensorEvent {
    pub(crate) device_proxy: InputDeviceProxy,
    pub(crate) rgbc: Rgbc<u16>,
}

impl PartialEq for LightSensorEvent {
    fn eq(&self, other: &Self) -> bool {
        self.rgbc == other.rgbc
    }
}

impl Eq for LightSensorEvent {}

impl LightSensorEvent {
    pub fn record_inspect(&self, node: &fuchsia_inspect::Node) {
        node.record_uint("red", u64::from(self.rgbc.red));
        node.record_uint("green", u64::from(self.rgbc.green));
        node.record_uint("blue", u64::from(self.rgbc.blue));
        node.record_uint("clear", u64::from(self.rgbc.clear));
    }
}

/// A [`LightSensorBinding`] represents a connection to a light sensor input device.
///
/// TODO more details
pub(crate) struct LightSensorBinding {
    /// The channel to stream InputEvents to.
    event_sender: UnboundedSender<input_device::InputEvent>,

    /// Holds information about this device.
    device_descriptor: LightSensorDeviceDescriptor,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LightSensorDeviceDescriptor {
    /// The vendor id of the connected light sensor input device.
    pub(crate) vendor_id: u32,

    /// The product id of the connected light sensor input device.
    pub(crate) product_id: u32,

    /// The device id of the connected light sensor input device.
    pub(crate) device_id: u32,

    /// Layout of the color channels in the sensor report.
    pub(crate) sensor_layout: Rgbc<usize>,
}

#[async_trait]
impl InputDeviceBinding for LightSensorBinding {
    fn input_event_sender(&self) -> UnboundedSender<InputEvent> {
        self.event_sender.clone()
    }

    fn get_device_descriptor(&self) -> InputDeviceDescriptor {
        InputDeviceDescriptor::LightSensor(self.device_descriptor.clone())
    }

    async fn handle_input_config_request(
        &self,
        _request: &InputConfigFeaturesRequest,
    ) -> Result<(), Error> {
        Ok(())
    }
}

impl LightSensorBinding {
    /// Creates a new [`InputDeviceBinding`] from the `device_proxy`.
    ///
    /// The binding will start listening for input reports immediately and send new InputEvents
    /// to the device binding owner over `input_event_sender`.
    ///
    /// # Parameters
    /// - `device_proxy`: The proxy to bind the new [`InputDeviceBinding`] to.
    /// - `device_id`: The unique identifier of this device.
    /// - `input_event_sender`: The channel to send new InputEvents to.
    /// - `device_node`: The inspect node for this device binding
    ///
    /// # Errors
    /// If there was an error binding to the proxy.
    pub(crate) async fn new(
        device_proxy: InputDeviceProxy,
        device_id: u32,
        input_event_sender: UnboundedSender<input_device::InputEvent>,
        device_node: fuchsia_inspect::Node,
    ) -> Result<Self, Error> {
        let (device_binding, mut inspect_status) =
            Self::bind_device(&device_proxy, device_id, input_event_sender, device_node).await?;
        inspect_status.health_node.set_ok();
        input_device::initialize_report_stream(
            device_proxy.clone(),
            device_binding.get_device_descriptor(),
            device_binding.input_event_sender(),
            inspect_status,
            move |report,
                  previous_report,
                  device_descriptor,
                  input_event_sender,
                  inspect_status| {
                Self::process_reports(
                    report,
                    previous_report,
                    device_descriptor,
                    input_event_sender,
                    device_proxy.clone(),
                    inspect_status,
                )
            },
        );

        Ok(device_binding)
    }

    /// Binds the provided input device to a new instance of `LightSensorBinding`.
    ///
    /// # Parameters
    /// - `device`: The device to use to initialize the binding.
    /// - `device_id`: The device ID being bound.
    /// - `input_event_sender`: The channel to send new InputEvents to.
    /// - `device_node`: The inspect node for this device binding
    ///
    /// # Errors
    /// If the device descriptor could not be retrieved, or the descriptor could not be parsed
    /// correctly.
    async fn bind_device(
        device: &InputDeviceProxy,
        device_id: u32,
        input_event_sender: UnboundedSender<input_device::InputEvent>,
        device_node: fuchsia_inspect::Node,
    ) -> Result<(Self, InputDeviceStatus), Error> {
        let mut input_device_status = InputDeviceStatus::new(device_node);
        let descriptor = match device.get_descriptor().await {
            Ok(descriptor) => descriptor,
            Err(_) => {
                input_device_status.health_node.set_unhealthy("Could not get device descriptor.");
                return Err(format_err!("Could not get descriptor for device_id: {}", device_id));
            }
        };
        let device_info = descriptor.device_info.ok_or_else(|| {
            input_device_status.health_node.set_unhealthy("Empty device_info in descriptor.");
            // Logging in addition to returning an error, as in some test
            // setups the error may never be displayed to the user.
            tracing::error!("DRIVER BUG: empty device_info for device_id: {}", device_id);
            format_err!("empty device info for device_id: {}", device_id)
        })?;
        match descriptor.sensor {
            Some(SensorDescriptor {
                input: Some(input_descriptors),
                feature: Some(features),
                ..
            }) => {
                let (_sensitivity, _sampling_rate, sensor_layout) = input_descriptors
                    .into_iter()
                    .zip(features.into_iter())
                    .filter_map(|(input_descriptor, feature)| {
                        input_descriptor
                            .values
                            .and_then(|values| {
                                let mut red_value = None;
                                let mut green_value = None;
                                let mut blue_value = None;
                                let mut clear_value = None;
                                for (i, value) in values.iter().enumerate() {
                                    let old = match value.type_ {
                                        SensorType::LightRed => {
                                            std::mem::replace(&mut red_value, Some(i))
                                        }
                                        SensorType::LightGreen => {
                                            std::mem::replace(&mut green_value, Some(i))
                                        }
                                        SensorType::LightBlue => {
                                            std::mem::replace(&mut blue_value, Some(i))
                                        }
                                        SensorType::LightIlluminance => {
                                            std::mem::replace(&mut clear_value, Some(i))
                                        }
                                        type_ => {
                                            tracing::warn!(
                                                "unexpected sensor type {type_:?} found on light \
                                                sensor device"
                                            );
                                            None
                                        }
                                    };
                                    if old.is_some() {
                                        tracing::warn!(
                                            "existing index for light sensor {:?} replaced",
                                            value.type_
                                        );
                                    }
                                }

                                red_value.and_then(|red| {
                                    green_value.and_then(|green| {
                                        blue_value.and_then(|blue| {
                                            clear_value.map(|clear| Rgbc {
                                                red,
                                                green,
                                                blue,
                                                clear,
                                            })
                                        })
                                    })
                                })
                            })
                            .and_then(|sensor_layout| {
                                feature.sampling_rate.and_then(|sampling_rate| {
                                    feature.sensitivity.map(|sensitivity| {
                                        (sensitivity, sampling_rate, sensor_layout)
                                    })
                                })
                            })
                    })
                    .next()
                    .ok_or_else(|| {
                        input_device_status.health_node.set_unhealthy("Missing light sensor data.");
                        format_err!("missing sensor data in device")
                    })?;
                Ok((
                    LightSensorBinding {
                        event_sender: input_event_sender,
                        device_descriptor: LightSensorDeviceDescriptor {
                            vendor_id: device_info.vendor_id,
                            product_id: device_info.product_id,
                            device_id,
                            sensor_layout,
                        },
                    },
                    input_device_status,
                ))
            }
            device_descriptor => {
                input_device_status
                    .health_node
                    .set_unhealthy("Light Sensor Device Descriptor failed to parse.");
                Err(format_err!(
                    "Light Sensor Device Descriptor failed to parse: \n {:?}",
                    device_descriptor
                ))
            }
        }
    }

    /// Parses an [`InputReport`] into one or more [`InputEvent`]s.
    ///
    /// The [`InputEvent`]s are sent to the device binding owner via [`input_event_sender`].
    ///
    /// # Parameters
    /// `report`: The incoming [`InputReport`].
    /// `previous_report`: The previous [`InputReport`] seen for the same device.
    /// `device_descriptor`: The descriptor for the input device generating the input reports.
    /// `input_event_sender`: The sender for the device binding's input event stream.
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
        device_proxy: InputDeviceProxy,
        inspect_status: &InputDeviceStatus,
    ) -> (Option<InputReport>, Option<UnboundedReceiver<InputEvent>>) {
        inspect_status.count_received_report(&report);
        let light_sensor_descriptor =
            if let input_device::InputDeviceDescriptor::LightSensor(ref light_sensor_descriptor) =
                device_descriptor
            {
                light_sensor_descriptor
            } else {
                unreachable!()
            };

        // Input devices can have multiple types so ensure `report` is a KeyboardInputReport.
        let sensor = match &report.sensor {
            None => {
                inspect_status.count_filtered_reports(1u64);
                return (previous_report, None);
            }
            Some(sensor) => sensor,
        };

        let values = match &sensor.values {
            None => {
                inspect_status.count_filtered_reports(1u64);
                return (None, None);
            }
            Some(values) => values,
        };

        let event = input_device::InputEvent {
            device_event: input_device::InputDeviceEvent::LightSensor(LightSensorEvent {
                device_proxy,
                rgbc: Rgbc {
                    red: values[light_sensor_descriptor.sensor_layout.red] as u16,
                    green: values[light_sensor_descriptor.sensor_layout.green] as u16,
                    blue: values[light_sensor_descriptor.sensor_layout.blue] as u16,
                    clear: values[light_sensor_descriptor.sensor_layout.clear] as u16,
                },
            }),
            device_descriptor: device_descriptor.clone(),
            event_time: zx::Time::get_monotonic(),
            handled: Handled::No,
            trace_id: None,
        };

        if let Err(e) = input_event_sender.unbounded_send(event.clone()) {
            tracing::error!("Failed to send LightSensorEvent with error: {e:?}");
        } else {
            inspect_status.count_generated_event(event);
        }

        (Some(report), None)
    }
}
