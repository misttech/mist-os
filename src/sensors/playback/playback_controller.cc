// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/sensors/playback/playback_controller.h"

#include <lib/fpromise/bridge.h>

namespace sensors::playback {
namespace {
using fpromise::bridge;
using fpromise::promise;
using fpromise::result;
using fpromise::scope;

using fuchsia_hardware_sensors::ActivateSensorError;
using fuchsia_hardware_sensors::ConfigurePlaybackError;
using fuchsia_hardware_sensors::ConfigureSensorRateError;
using fuchsia_hardware_sensors::DeactivateSensorError;
using fuchsia_hardware_sensors::FixedValuesPlaybackConfig;
using fuchsia_hardware_sensors::PlaybackSourceConfig;
using fuchsia_sensors_types::SensorEvent;
using fuchsia_sensors_types::SensorId;
using fuchsia_sensors_types::SensorInfo;
using fuchsia_sensors_types::SensorRateConfig;
}  // namespace

PlaybackController::PlaybackController(async_dispatcher_t* dispatcher)
    : ActorBase(dispatcher, scope_), running_playback_scope_(std::make_unique<scope>()) {}

promise<void, ConfigurePlaybackError> PlaybackController::ConfigurePlayback(
    const PlaybackSourceConfig& config) {
  bridge<void, ConfigurePlaybackError> bridge;
  Schedule([this, config, completer = std::move(bridge.completer)]() mutable -> promise<void> {
    switch (config.Which()) {
      case PlaybackSourceConfig::Tag::kFixedValuesConfig:
        // Stop any playback and clear the playback configuration.
        return StopScheduledPlayback()
            .and_then(ClearPlaybackConfig())
            .then([this, config](result<void>& result) -> promise<void, ConfigurePlaybackError> {
              std::optional<FixedValuesPlaybackConfig> fixed_config = config.fixed_values_config();
              return ConfigureFixedValues(*fixed_config);
            })
            .then([completer = std::move(completer)](
                      result<void, ConfigurePlaybackError>& result) mutable -> promise<void> {
              if (result.is_ok()) {
                completer.complete_ok();
              } else {
                completer.complete_error(result.error());
              }
              return fpromise::make_ok_promise();
            });
        break;
      default:
        // Complete the bridged promise with an error and do nothing further.
        completer.complete_error(ConfigurePlaybackError::kInvalidConfigType);
        return fpromise::make_ok_promise();
    };
  });
  return bridge.consumer.promise();
}

promise<std::vector<SensorInfo>> PlaybackController::GetSensorsList() {
  bridge<std::vector<SensorInfo>> bridge;
  Schedule([this, completer = std::move(bridge.completer)]() mutable {
    completer.complete_ok(sensor_list_);
  });
  return bridge.consumer.promise();
}

promise<void, ActivateSensorError> PlaybackController::ActivateSensor(SensorId sensor_id) {
  bridge<void, ActivateSensorError> bridge;
  Schedule(fpromise::make_promise(
      [this, sensor_id, completer = std::move(bridge.completer)]() mutable -> promise<void> {
        if (sensor_playback_state_.count(sensor_id) == 0) {
          FX_LOGS(ERROR) << "ActivateSensor error, invalid sensor ID.";
          completer.complete_error(ActivateSensorError::kInvalidSensorId);
          return fpromise::make_ok_promise();
        }

        return UpdatePlaybackState(sensor_id, true)
            .and_then([completer = std::move(completer)]() mutable { completer.complete_ok(); });
      }));

  return bridge.consumer.promise();
}

promise<void, DeactivateSensorError> PlaybackController::DeactivateSensor(SensorId sensor_id) {
  bridge<void, DeactivateSensorError> bridge;
  Schedule(fpromise::make_promise(
      [this, sensor_id, completer = std::move(bridge.completer)]() mutable -> promise<void> {
        if (sensor_playback_state_.count(sensor_id) == 0) {
          FX_LOGS(ERROR) << "DeactivateSensor error, invalid sensor ID.";
          completer.complete_error(DeactivateSensorError::kInvalidSensorId);
          return fpromise::make_ok_promise();
        }

        return UpdatePlaybackState(sensor_id, false)
            .and_then([completer = std::move(completer)]() mutable { completer.complete_ok(); });
      }));

  return bridge.consumer.promise();
}

promise<void, ConfigureSensorRateError> PlaybackController::ConfigureSensorRate(
    SensorId sensor_id, SensorRateConfig rate_config) {
  bridge<void, ConfigureSensorRateError> bridge;
  Schedule([this, sensor_id, rate_config, completer = std::move(bridge.completer)]() mutable {
    if (!rate_config.sampling_period_ns() || !rate_config.max_reporting_latency_ns()) {
      FX_LOGS(ERROR) << "ConfigureSensorRate: Fields missing from rate config.";
      completer.complete_error(ConfigureSensorRateError::kInvalidConfig);
      return;
    }
    if (sensor_playback_state_.count(sensor_id) == 0) {
      FX_LOGS(ERROR) << "ConfigureSensorRate: Invalid sensor ID.";
      completer.complete_error(ConfigureSensorRateError::kInvalidSensorId);
      return;
    }

    FX_LOGS(INFO) << "ConfigureSensorRate: Setting sampling period to "
                  << *rate_config.sampling_period_ns() << " ns and max reporting lagency to "
                  << *rate_config.max_reporting_latency_ns() << " ns for sensor " << sensor_id;

    sensor_playback_state_[sensor_id].sampling_period =
        zx::duration(*rate_config.sampling_period_ns());
    sensor_playback_state_[sensor_id].max_reporting_latency =
        zx::duration(*rate_config.max_reporting_latency_ns());
    completer.complete_ok();
  });
  return bridge.consumer.promise();
}

promise<void> PlaybackController::SetEventCallback(
    std::function<void(const SensorEvent&)> event_callback) {
  bridge<void> bridge;
  Schedule([this, event_callback = std::move(event_callback),
            completer = std::move(bridge.completer)]() mutable {
    event_callback_ = std::move(event_callback);
    completer.complete_ok();
  });
  return bridge.consumer.promise();
}

void PlaybackController::AdoptSensorList(const std::vector<SensorInfo>& sensor_list) {
  sensor_list_ = sensor_list;
  for (const SensorInfo& info : sensor_list_) {
    sensor_playback_state_[*info.sensor_id()] = SensorPlaybackState();
  }
}

promise<void> PlaybackController::ClearPlaybackConfig() {
  return fpromise::make_promise([this]() {
    sensor_list_.clear();

    playback_state_ = PlaybackState::kStopped;
    sensor_playback_state_.clear();
    enabled_sensor_count_ = 0;
    playback_mode_ = PlaybackMode::kFixedValuesMode;
  });
}

promise<void, ConfigurePlaybackError> PlaybackController::ConfigureFixedValues(
    const FixedValuesPlaybackConfig& config) {
  return fpromise::make_promise([this, config]() mutable -> promise<void, ConfigurePlaybackError> {
    FX_LOGS(INFO) << "ConfigurePlayback: Configuring to emit a repeating fixed list.";
    // Check for required fields.
    std::optional<ConfigurePlaybackError> result = ValidateSensorList(config.sensor_list());
    if (result) {
      FX_LOGS(ERROR) << "ConfigurePlayback: Received invalid or incomplete sensor list.";
      return fpromise::make_error_promise(*result);
    }
    result = ValidateSensorEventList(config.sensor_events(), *config.sensor_list());
    if (result) {
      FX_LOGS(ERROR) << "ConfigurePlayback: Received sensor event list with invalid or "
                     << "incomplete entries.";
      return fpromise::make_error_promise(*result);
    }

    playback_mode_ = PlaybackMode::kFixedValuesMode;
    AdoptSensorList(*config.sensor_list());

    // Store the events on a per-sensor basis.
    for (const SensorEvent& event : *config.sensor_events()) {
      sensor_playback_state_[event.sensor_id()].fixed_sensor_events.push_back(event);
    }

    return fpromise::make_result_promise<void, ConfigurePlaybackError>(fpromise::ok());
  });
}

promise<void> PlaybackController::UpdatePlaybackState(SensorId sensor_id, bool enabled) {
  return fpromise::make_promise([this, sensor_id, enabled]() -> promise<void> {
    SensorPlaybackState& state = sensor_playback_state_[sensor_id];
    bool already_enabled = state.enabled;
    state.enabled = enabled;

    if (!already_enabled && enabled) {
      FX_LOGS(INFO) << "ActivateSensor: Enabling sensor " << sensor_id;
      enabled_sensor_count_ += 1;
    } else if (already_enabled && !enabled) {
      FX_LOGS(INFO) << "ActivateSensor: Disabling sensor " << sensor_id;
      enabled_sensor_count_ -= 1;
    } else if (!already_enabled && !enabled) {
      FX_LOGS(WARNING) << "DeactivateSensor: Sensor " << sensor_id << " already disabled.";
      return fpromise::make_ok_promise();
    } else if (already_enabled && enabled) {
      FX_LOGS(WARNING) << "ActivateSensor: Sensor " << sensor_id << " already enabled.";
      return fpromise::make_ok_promise();
    }
    ZX_ASSERT(enabled_sensor_count_ >= 0);

    if (playback_state_ == PlaybackState::kStopped) {
      // If the overall state is stopped but there are enabled sensors, start playback.
      if (enabled_sensor_count_ > 0) {
        FX_LOGS(INFO) << "Starting sensor playback.";

        playback_state_ = PlaybackState::kRunning;
      }
    } else if (playback_state_ == PlaybackState::kRunning) {
      // If the overall state is running but there are no sensors enabled, stop playback.
      if (enabled_sensor_count_ == 0) {
        FX_LOGS(INFO) << "Stopping sensor playback.";

        playback_state_ = PlaybackState::kStopped;
        return StopScheduledPlayback();
      }
    }

    if (enabled) {
      return ScheduleSensorEvents(sensor_id);
    } else {
      return fpromise::make_ok_promise();
    }
  });
}

promise<void> PlaybackController::ScheduleSensorEvents(SensorId sensor_id) {
  return fpromise::make_promise([this, sensor_id]() -> promise<void> {
    switch (playback_mode_) {
      case PlaybackMode::kFixedValuesMode:
        return ScheduleFixedEvent(sensor_id, /*first_event=*/true);
    }
  });
}

// Generates the next event for a sensor given the current internal state of the fixed playback
// sequence.
// This should only be called from a promise scheduled to run on this actor's executor.
SensorEvent PlaybackController::GenerateNextFixedEventForSensor(SensorId sensor_id,
                                                                zx::time timestamp) {
  SensorPlaybackState& state = sensor_playback_state_[sensor_id];

  SensorEvent first_event = state.fixed_sensor_events[state.next_fixed_event];
  state.next_fixed_event = (state.next_fixed_event + 1) % state.fixed_sensor_events.size();

  state.last_scheduled_event_time = timestamp;
  first_event.timestamp(timestamp.get());

  return first_event;
}

promise<void> PlaybackController::ScheduleFixedEvent(SensorId sensor_id, bool first_event) {
  return fpromise::make_promise([this, sensor_id, first_event]() {
    // If playback shouldn't be running any more, schedule nothing further.
    if (playback_state_ != PlaybackState::kRunning) {
      return;
    }

    SensorPlaybackState& state = sensor_playback_state_[sensor_id];
    // If the sensor has become disabled, schedule nothing further.
    if (!state.enabled) {
      return;
    }

    // Schedules a promise chain which will emit an event, and then schedule the sending of the
    // following event for this sensor. If it's the first event after activation schedule it
    // immediately, otherwise schedule it for the previous event time plus the sampling period.
    //
    // Wraps the promise chain in a special scope so we can cancel it separately to the rest of the
    // controller actor promises.
    if (first_event) {
      zx::time now = zx::time(zx_clock_get_monotonic());
      SensorEvent first_event = GenerateNextFixedEventForSensor(sensor_id, now);
      Schedule(SendEvent(first_event)
                   .and_then(ScheduleFixedEvent(sensor_id, /*first_event=*/false))
                   .wrap_with(*running_playback_scope_));
    } else {
      zx::time event_timestamp = state.last_scheduled_event_time + state.sampling_period;
      SensorEvent next_event = GenerateNextFixedEventForSensor(sensor_id, event_timestamp);
      ScheduleAtTime(event_timestamp,
                     SendEvent(next_event)
                         .and_then(ScheduleFixedEvent(sensor_id, /*first_event=*/false))
                         .wrap_with(*running_playback_scope_));
    }
  });
}

promise<void> PlaybackController::SendEvent(const SensorEvent& event) {
  return fpromise::make_promise([this, event]() {
    // If playback shouldn't be running any more, do nothing.
    if (playback_state_ != PlaybackState::kRunning)
      return;
    // If the sensor has become disabled, do nothing.
    if (!sensor_playback_state_[event.sensor_id()].enabled)
      return;

    event_callback_(event);
  });
}

promise<void> PlaybackController::StopScheduledPlayback() {
  return fpromise::make_promise([this]() { running_playback_scope_ = std::make_unique<scope>(); });
}

}  // namespace sensors::playback
