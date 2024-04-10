// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_SENSORS_PLAYBACK_PLAYBACK_CONTROLLER_H_
#define SRC_SENSORS_PLAYBACK_PLAYBACK_CONTROLLER_H_

#include <fidl/fuchsia.hardware.sensors/cpp/fidl.h>
#include <lib/fit/function.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/scope.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/clock.h>

#include <queue>
#include <unordered_map>
#include <vector>

#include "src/camera/lib/actor/actor_base.h"
#include "src/sensors/playback/playback_config_validation.h"

namespace sensors::playback {

class PlaybackController : public camera::actor::ActorBase {
  using ActivateSensorError = fuchsia_hardware_sensors::ActivateSensorError;
  using DeactivateSensorError = fuchsia_hardware_sensors::DeactivateSensorError;
  using ConfigurePlaybackError = fuchsia_hardware_sensors::ConfigurePlaybackError;
  using ConfigureSensorRateError = fuchsia_hardware_sensors::ConfigureSensorRateError;
  using FixedValuesPlaybackConfig = fuchsia_hardware_sensors::FixedValuesPlaybackConfig;
  using PlaybackSourceConfig = fuchsia_hardware_sensors::PlaybackSourceConfig;
  using SensorEvent = fuchsia_sensors_types::SensorEvent;
  using SensorId = fuchsia_sensors_types::SensorId;
  using SensorInfo = fuchsia_sensors_types::SensorInfo;
  using SensorRateConfig = fuchsia_sensors_types::SensorRateConfig;

  static constexpr auto kLowerTimestamp = [](const SensorEvent& l, const SensorEvent& r) {
    return l.timestamp() > r.timestamp();
  };
  using EventPriorityQueue =
      std::priority_queue<SensorEvent, std::vector<SensorEvent>, decltype(kLowerTimestamp)>;

 public:
  explicit PlaybackController(async_dispatcher_t* dispatcher);

  fpromise::promise<void, ConfigurePlaybackError> ConfigurePlayback(
      const PlaybackSourceConfig& config);

  fpromise::promise<std::vector<SensorInfo>> GetSensorsList();

  fpromise::promise<void, ActivateSensorError> ActivateSensor(SensorId sensor_id);

  fpromise::promise<void, DeactivateSensorError> DeactivateSensor(SensorId sensor_id);

  fpromise::promise<void, ConfigureSensorRateError> ConfigureSensorRate(
      SensorId sensor_id, SensorRateConfig rate_config);

  fpromise::promise<void> SetEventCallback(std::function<void(const SensorEvent&)> event_callback);

 private:
  constexpr static int64_t kDefaultSamplingPeriodNs = 1e9;     // 1 second.
  constexpr static int64_t kDefaultMaxReportingLatencyNs = 0;  // No buffering.

  enum class PlaybackMode : std::uint8_t {
    kFixedValuesMode,
  };

  enum class PlaybackState : std::uint8_t { kStopped, kRunning };

  struct SensorPlaybackState {
    bool enabled = false;
    zx::duration sampling_period = zx::duration(kDefaultSamplingPeriodNs);
    zx::duration max_reporting_latency = zx::duration(kDefaultMaxReportingLatencyNs);
    zx::time last_scheduled_event_time = zx::time::infinite_past();

    // For fixed value playback
    std::vector<SensorEvent> fixed_sensor_events;
    int next_fixed_event = 0;
  };

  // Populate the sensor lists for this playback component with the sensors from the given list.
  // This should only be called from a promise scheduled to run on this actor's executor.
  void AdoptSensorList(const std::vector<SensorInfo>& sensor_list);

  fpromise::promise<void> ClearPlaybackConfig();

  fpromise::promise<void, ConfigurePlaybackError> ConfigureFixedValues(
      const FixedValuesPlaybackConfig& config);

  fpromise::promise<void> UpdatePlaybackState(SensorId sensor_id, bool enabled);

  fpromise::promise<void> ScheduleSensorEvents(SensorId sensor_id);

  // Generates the next event for a sensor given the current internal state of the fixed playback
  // sequence.
  // This should only be called from a promise scheduled to run on this actor's executor.
  SensorEvent GenerateNextFixedEventForSensor(SensorId sensor_id, zx::time timestamp);

  fpromise::promise<void> ScheduleFixedEvent(SensorId sensor_id, bool first_event);

  fpromise::promise<void> SendEvent(const SensorEvent& event);

  fpromise::promise<void> StopScheduledPlayback();

  // Callback to call with sensor events.
  std::function<void(const SensorEvent&)> event_callback_;

  // List of "sensors" currently provided by the playback data.
  std::vector<SensorInfo> sensor_list_;

  // Overall playback state.
  PlaybackState playback_state_ = PlaybackState::kStopped;
  // Playback states for each sensor.
  std::unordered_map<SensorId, SensorPlaybackState> sensor_playback_state_;

  // Number of currently enabled sensors.
  int enabled_sensor_count_ = 0;

  // Which playback mode is currently active.
  PlaybackMode playback_mode_;

  // A promise scope to wrap scheduled SendEvent and scheduling promises.
  std::unique_ptr<fpromise::scope> running_playback_scope_;

  // This should always be the last thing in the object. Otherwise scheduled tasks within this scope
  // which reference members of this object may be allowed to run after destruction of this object
  // has started. Keeping this at the end ensures that the scope is destroyed first, cancelling any
  // scheduled tasks before the rest of the members are destroyed.
  fpromise::scope scope_;
};

}  // namespace sensors::playback

#endif  // SRC_SENSORS_PLAYBACK_PLAYBACK_CONTROLLER_H_
