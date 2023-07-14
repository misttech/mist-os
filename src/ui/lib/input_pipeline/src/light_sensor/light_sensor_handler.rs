// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::input_device::{Handled, InputDeviceDescriptor, InputDeviceEvent, InputEvent};
use crate::input_handler::{InputHandler, InputHandlerStatus};
use crate::light_sensor::calibrator::{Calibrate, Calibrator};
use crate::light_sensor::led_watcher::{CancelableTask, LedWatcher, LedWatcherHandle};
use crate::light_sensor::types::{AdjustmentSetting, Calibration, Rgbc, SensorConfiguration};
use anyhow::{format_err, Context, Error};
use async_trait::async_trait;
use async_utils::hanging_get::server::HangingGet;
use fidl_fuchsia_input_report::{FeatureReport, InputDeviceProxy, SensorFeatureReport};
use fidl_fuchsia_lightsensor::{
    LightSensorData as FidlLightSensorData, Rgbc as FidlRgbc, SensorRequest, SensorRequestStream,
    SensorWatchResponder,
};
use fidl_fuchsia_settings::LightProxy;
use fidl_fuchsia_ui_brightness::ControlProxy as BrightnessControlProxy;
use fuchsia_inspect;
use fuchsia_zircon as zx;
use futures::channel::oneshot;
use futures::TryStreamExt;
use std::cell::RefCell;
use std::rc::Rc;

type NotifyFn = Box<dyn Fn(&LightSensorData, SensorWatchResponder) -> bool>;
type SensorHangingGet = HangingGet<LightSensorData, SensorWatchResponder, NotifyFn>;

// Precise value is 2.78125ms, but data sheet lists 2.78ms.
/// Number of us for each cycle of the sensor.
const MIN_TIME_STEP_US: u32 = 2780;
/// Maximum multiplier.
const MAX_GAIN: u32 = 64;
/// Maximum sensor reading per cycle for any 1 color channel.
const MAX_COUNT_PER_CYCLE: u32 = 1024;
/// Absolute maximum reading the sensor can return for any 1 color channel.
const MAX_SATURATION: u32 = u16::MAX as u32;
const MAX_ATIME: u32 = 256;
/// Driver scales the values by max gain & atime in ms.
const ADC_SCALING_FACTOR: f32 = 64.0 * 256.0;
/// The gain up margin should be 10% of the saturation point.
const GAIN_UP_MARGIN_DIVISOR: u32 = 10;
/// The divisor for scaling uncalibrated values to transition old clients to auto gain.
const TRANSITION_SCALING_FACTOR: f32 = 4.0;

#[derive(Copy, Clone, Debug)]
struct LightReading {
    rgbc: Rgbc<f32>,
    si_rgbc: Rgbc<f32>,
    is_calibrated: bool,
    lux: f32,
    cct: f32,
}

fn num_cycles(atime: u32) -> u32 {
    MAX_ATIME - atime
}

struct ActiveSetting {
    settings: Vec<AdjustmentSetting>,
    idx: usize,
}

impl ActiveSetting {
    fn new(settings: Vec<AdjustmentSetting>, idx: usize) -> Self {
        Self { settings, idx }
    }

    async fn adjust(
        &mut self,
        reading: Rgbc<u16>,
        device_proxy: InputDeviceProxy,
    ) -> Result<(), SaturatedError> {
        let saturation_point =
            (num_cycles(self.active_setting().atime) * MAX_COUNT_PER_CYCLE).min(MAX_SATURATION);
        let gain_up_margin = saturation_point / GAIN_UP_MARGIN_DIVISOR;

        let step_change = self.step_change();
        let mut pull_up = true;

        if saturated(reading) {
            if self.adjust_down() {
                tracing::info!("adjusting down due to saturation sentinel");
                self.update_device(device_proxy).await.context("updating light sensor device")?;
            }
            return Err(SaturatedError::Saturated);
        }

        for value in [reading.red, reading.green, reading.blue, reading.clear] {
            let value = value as u32;
            if value >= saturation_point {
                if self.adjust_down() {
                    tracing::info!("adjusting down due to saturation point");
                    self.update_device(device_proxy)
                        .await
                        .context("updating light sensor device")?;
                }
                return Err(SaturatedError::Saturated);
            } else if (value * step_change + gain_up_margin) >= saturation_point {
                pull_up = false;
            }
        }

        if pull_up {
            if self.adjust_up() {
                tracing::info!("adjusting up");
                self.update_device(device_proxy).await.context("updating light sensor device")?;
            }
        }

        Ok(())
    }

    async fn update_device(&self, device_proxy: InputDeviceProxy) -> Result<(), Error> {
        let active_setting = self.active_setting();
        let feature_report = device_proxy
            .get_feature_report()
            .await
            .context("calling get_feature_report")?
            .map_err(|e| {
                format_err!(
                    "getting feature report on light sensor device: {:?}",
                    zx::Status::from_raw(e),
                )
            })?;
        device_proxy
            .set_feature_report(&FeatureReport {
                sensor: Some(SensorFeatureReport {
                    sensitivity: Some(vec![active_setting.gain as i64]),
                    // Feature report expects sampling rate in microseconds.
                    sampling_rate: Some(to_us(active_setting.atime) as i64),
                    ..(feature_report
                        .sensor
                        .ok_or_else(|| format_err!("missing sensor in feature_report"))?)
                }),
                ..feature_report
            })
            .await
            .context("calling set_feature_report")?
            .map_err(|e| {
                format_err!(
                    "updating feature report on light sensor device: {:?}",
                    zx::Status::from_raw(e),
                )
            })
    }

    fn active_setting(&self) -> AdjustmentSetting {
        self.settings[self.idx]
    }

    /// Adjusts to a lower setting. Returns whether or not the setting changed.
    fn adjust_down(&mut self) -> bool {
        if self.idx == 0 {
            false
        } else {
            tracing::info!("adjusting down");
            self.idx -= 1;
            true
        }
    }

    /// Calculate the effect to saturation that occurs by moving the setting up a step.
    fn step_change(&self) -> u32 {
        let current = self.active_setting();
        let new = match self.settings.get(self.idx + 1) {
            Some(setting) => *setting,
            // If we're at the limit, just return a coefficient of 1 since there will be no step
            // change.
            None => return 1,
        };
        div_round_up(new.gain, current.gain) * div_round_up(to_us(new.atime), to_us(current.atime))
    }

    /// Adjusts to a higher setting. Returns whether or not the setting changed.
    fn adjust_up(&mut self) -> bool {
        if self.idx == self.settings.len() - 1 {
            false
        } else {
            tracing::info!("adjusting up");
            self.idx += 1;
            true
        }
    }
}

pub struct LightSensorHandler<T> {
    hanging_get: RefCell<SensorHangingGet>,
    calibrator: Option<T>,
    active_setting: RefCell<ActiveSettingState>,
    rgbc_to_lux_coefs: Rgbc<f32>,
    si_scaling_factors: Rgbc<f32>,
    vendor_id: u32,
    product_id: u32,
    /// The inventory of this handler's Inspect status.
    inspect_status: InputHandlerStatus,
}

enum ActiveSettingState {
    Uninitialized(Vec<AdjustmentSetting>),
    Initialized(ActiveSetting),
}

pub type CalibratedLightSensorHandler = LightSensorHandler<Calibrator<LedWatcherHandle>>;
pub async fn make_light_sensor_handler_and_spawn_led_watcher(
    light_proxy: LightProxy,
    brightness_proxy: BrightnessControlProxy,
    calibration: Option<Calibration>,
    configuration: SensorConfiguration,
    input_handlers_node: &fuchsia_inspect::Node,
) -> Result<(Rc<CalibratedLightSensorHandler>, Option<CancelableTask>), Error> {
    let (calibrator, watcher_task) = if let Some(calibration) = calibration {
        let light_groups =
            light_proxy.watch_light_groups().await.context("request initial light groups")?;
        let led_watcher = LedWatcher::new(light_groups);
        let (cancelation_tx, cancelation_rx) = oneshot::channel();
        let (led_watcher_handle, watcher_task) = led_watcher
            .handle_light_groups_and_brightness_watch(
                light_proxy,
                brightness_proxy,
                cancelation_rx,
            );
        let watcher_task = CancelableTask::new(cancelation_tx, watcher_task);
        let calibrator = Calibrator::new(calibration, led_watcher_handle);
        (Some(calibrator), Some(watcher_task))
    } else {
        (None, None)
    };
    Ok((LightSensorHandler::new(calibrator, configuration, input_handlers_node), watcher_task))
}

impl<T> LightSensorHandler<T> {
    pub fn new(
        calibrator: impl Into<Option<T>>,
        configuration: SensorConfiguration,
        input_handlers_node: &fuchsia_inspect::Node,
    ) -> Rc<Self> {
        let calibrator = calibrator.into();
        let hanging_get = RefCell::new(HangingGet::new_unknown_state(Box::new(
            |sensor_data: &LightSensorData, responder: SensorWatchResponder| -> bool {
                if let Err(e) = responder.send(&FidlLightSensorData::from(*sensor_data)) {
                    tracing::warn!("Failed to send updated data to client: {e:?}",);
                }
                true
            },
        ) as NotifyFn));
        let active_setting =
            RefCell::new(ActiveSettingState::Uninitialized(configuration.settings));
        let inspect_status = InputHandlerStatus::new(
            input_handlers_node,
            "light_sensor_handler",
            /* generates_events */ false,
        );
        Rc::new(Self {
            hanging_get,
            calibrator,
            active_setting,
            rgbc_to_lux_coefs: configuration.rgbc_to_lux_coefficients,
            si_scaling_factors: configuration.si_scaling_factors,
            vendor_id: configuration.vendor_id,
            product_id: configuration.product_id,
            inspect_status,
        })
    }

    pub async fn handle_light_sensor_request_stream(
        self: &Rc<Self>,
        mut stream: SensorRequestStream,
    ) -> Result<(), Error> {
        let subscriber = self.hanging_get.borrow_mut().new_subscriber();
        while let Some(request) =
            stream.try_next().await.context("Error handling light sensor request stream")?
        {
            match request {
                SensorRequest::Watch { responder } => {
                    subscriber
                        .register(responder)
                        .context("registering responder for Watch call")?;
                }
            }
        }

        Ok(())
    }

    /// Calculates the lux of a reading.
    fn calculate_lux(&self, reading: Rgbc<f32>) -> f32 {
        Rgbc::multi_map(reading, self.rgbc_to_lux_coefs, |reading, coef| reading * coef)
            .fold(0.0, |lux, c| lux + c)
    }
}

/// Normalize raw sensor counts.
///
/// I.e. values being read in dark lighting will be returned as their original value,
/// but values in the brighter lighting will be returned larger, as a reading within the true
/// output range of the light sensor.
fn process_reading(reading: Rgbc<u16>, initial_setting: AdjustmentSetting) -> Rgbc<f32> {
    let gain_bias = MAX_GAIN / initial_setting.gain as u32;

    reading.map(|v| {
        div_round_closest(v as u32 * gain_bias * MAX_ATIME, num_cycles(initial_setting.atime))
            as f32
    })
}

#[derive(Debug)]
enum SaturatedError {
    Saturated,
    Anyhow(Error),
}

impl From<Error> for SaturatedError {
    fn from(value: Error) -> Self {
        Self::Anyhow(value)
    }
}

impl<T> LightSensorHandler<T>
where
    T: Calibrate,
{
    async fn get_calibrated_data(
        &self,
        reading: Rgbc<u16>,
        device_proxy: InputDeviceProxy,
    ) -> Result<LightReading, SaturatedError> {
        // Update the sensor after the active setting has been used for calculations, since it may
        // change after this call.
        let initial_setting = {
            let mut active_setting_state = self.active_setting.borrow_mut();
            match &mut *active_setting_state {
                ActiveSettingState::Uninitialized(ref mut adjustment_settings) => {
                    // Initial setting is unset. Reading cannot be properly adjusted, so override
                    // the current settings on the device and report a saturated error so this
                    // reading is not sent to any clients.
                    let active_setting = ActiveSetting::new(std::mem::take(adjustment_settings), 0);
                    active_setting
                        .update_device(device_proxy)
                        .await
                        .context("Unable to set initial settings for sensor")?;
                    *active_setting_state = ActiveSettingState::Initialized(active_setting);
                    return Err(SaturatedError::Saturated);
                }
                ActiveSettingState::Initialized(ref mut active_setting) => {
                    let initial_setting = active_setting.active_setting();
                    active_setting.adjust(reading, device_proxy).await.map_err(|e| match e {
                        SaturatedError::Saturated => SaturatedError::Saturated,
                        SaturatedError::Anyhow(e) => {
                            SaturatedError::Anyhow(e.context("adjusting active setting"))
                        }
                    })?;
                    initial_setting
                }
            }
        };
        let uncalibrated_rgbc = process_reading(reading, initial_setting);
        let rgbc = self
            .calibrator
            .as_ref()
            .map(|calibrator| calibrator.calibrate(uncalibrated_rgbc))
            .unwrap_or(uncalibrated_rgbc);

        let si_rgbc = (self.si_scaling_factors * rgbc).map(|c| c / ADC_SCALING_FACTOR);
        let lux = self.calculate_lux(si_rgbc);
        let cct = correlated_color_temperature(si_rgbc)?;

        let rgbc = uncalibrated_rgbc.map(|c| c as f32 / TRANSITION_SCALING_FACTOR);
        Ok(LightReading { rgbc, si_rgbc, is_calibrated: self.calibrator.is_some(), lux, cct })
    }
}

/// Converts atime values to microseconds.
fn to_us(atime: u32) -> u32 {
    num_cycles(atime) * MIN_TIME_STEP_US
}

/// Divides n by d, rounding up.
fn div_round_up(n: u32, d: u32) -> u32 {
    (n + d - 1) / d
}

/// Divides n by d, rounding to the closest value.
fn div_round_closest(n: u32, d: u32) -> u32 {
    (n + (d / 2)) / d
}

// These values are defined in //src/devices/light-sensor/ams-light/tcs3400.cc
const MAX_SATURATION_RED: u16 = 21_067;
const MAX_SATURATION_GREEN: u16 = 20_395;
const MAX_SATURATION_BLUE: u16 = 20_939;
const MAX_SATURATION_CLEAR: u16 = 65_085;

// TODO(http://fxbug.dev/65167) Update when sensor reports include saturation
// information.
fn saturated(reading: Rgbc<u16>) -> bool {
    reading.red == MAX_SATURATION_RED
        && reading.green == MAX_SATURATION_GREEN
        && reading.blue == MAX_SATURATION_BLUE
        && reading.clear == MAX_SATURATION_CLEAR
}

// See http://ams.com/eng/content/view/download/145158 for the detail of the
// following calculation.
fn correlated_color_temperature(reading: Rgbc<f32>) -> Result<f32, SaturatedError> {
    // TODO(https://fxbug.dev/121854): Move color_temp calculation out of common code
    let big_x = -0.7687 * reading.red + 9.7764 * reading.green + -7.4164 * reading.blue;
    let big_y = -1.7475 * reading.red + 9.9603 * reading.green + -5.6755 * reading.blue;
    let big_z = -3.6709 * reading.red + 4.8637 * reading.green + 4.3682 * reading.blue;

    let div = big_x + big_y + big_z;
    if div.abs() < f32::EPSILON {
        return Err(SaturatedError::Saturated);
    }

    let x = big_x / div;
    let y = big_y / div;
    let n = (x - 0.3320) / (0.1858 - y);
    Ok(449.0 * n.powi(3) + 3525.0 * n.powi(2) + 6823.3 * n + 5520.33)
}

#[async_trait(?Send)]
impl<T> InputHandler for LightSensorHandler<T>
where
    T: Calibrate + 'static,
{
    async fn handle_input_event(self: Rc<Self>, mut input_event: InputEvent) -> Vec<InputEvent> {
        if let InputEvent {
            device_event: InputDeviceEvent::LightSensor(ref light_sensor_event),
            device_descriptor: InputDeviceDescriptor::LightSensor(ref light_sensor_descriptor),
            event_time: _,
            handled: Handled::No,
            trace_id: _,
        } = input_event
        {
            self.inspect_status.count_received_event(input_event.clone());
            // Validate descriptor matches.
            if !(light_sensor_descriptor.vendor_id == self.vendor_id
                && light_sensor_descriptor.product_id == self.product_id)
            {
                // Don't handle the event.
                tracing::warn!(
                    "Unexpected device in light sensor handler: {:?}",
                    light_sensor_descriptor,
                );
                return vec![input_event];
            }
            let LightReading { rgbc, si_rgbc, is_calibrated, lux, cct } = match self
                .get_calibrated_data(
                    light_sensor_event.rgbc,
                    light_sensor_event.device_proxy.clone(),
                )
                .await
            {
                Ok(data) => data,
                Err(SaturatedError::Saturated) => {
                    // Event is handled but saturated data is not useful for clients. Mark as
                    // handled but do not publish data.
                    input_event.handled = Handled::Yes;
                    self.inspect_status.count_handled_event();
                    return vec![input_event];
                }
                Err(SaturatedError::Anyhow(e)) => {
                    tracing::warn!("Failed to get light sensor readings: {e:?}");
                    // Don't handle the event.
                    return vec![input_event];
                }
            };
            let publisher = self.hanging_get.borrow_mut().new_publisher();
            publisher.set(LightSensorData {
                rgbc,
                si_rgbc,
                is_calibrated,
                calculated_lux: lux,
                correlated_color_temperature: cct,
            });
            input_event.handled = Handled::Yes;
            self.inspect_status.count_handled_event();
        }
        vec![input_event]
    }
}

#[derive(Copy, Clone, PartialEq)]
struct LightSensorData {
    rgbc: Rgbc<f32>,
    si_rgbc: Rgbc<f32>,
    is_calibrated: bool,
    calculated_lux: f32,
    correlated_color_temperature: f32,
}

impl From<LightSensorData> for FidlLightSensorData {
    fn from(data: LightSensorData) -> Self {
        Self {
            rgbc: Some(FidlRgbc::from(data.rgbc)),
            si_rgbc: Some(FidlRgbc::from(data.si_rgbc)),
            is_calibrated: Some(data.is_calibrated),
            calculated_lux: Some(data.calculated_lux),
            correlated_color_temperature: Some(data.correlated_color_temperature),
            ..Default::default()
        }
    }
}

impl From<Rgbc<f32>> for FidlRgbc {
    fn from(rgbc: Rgbc<f32>) -> Self {
        Self {
            red_intensity: rgbc.red,
            green_intensity: rgbc.green,
            blue_intensity: rgbc.blue,
            clear_intensity: rgbc.clear,
        }
    }
}

#[cfg(test)]
mod light_sensor_handler_tests;
