// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_UNITTEST_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_UNITTEST_H_

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.mediastreams/cpp/natural_types.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fidl/cpp/unified_messaging_declarations.h>
#include <lib/fidl/cpp/wire/status.h>
#include <zircon/system/public/zircon/errors.h>

#include <iomanip>
#include <memory>
#include <string>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/media/audio/lib/clock/clock.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/testing/fake_audio_driver.h"
#include "src/media/audio/services/device_registry/testing/fake_device_presence_watcher.h"

namespace media_audio {

// Test class to verify the driver initialization/configuration sequence.
class DeviceTestBase : public gtest::TestLoopFixture {
 public:
  void SetUp() override {
    zx::channel server_end, client_end;
    ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));

    fake_device_presence_watcher_ = std::make_shared<FakeDevicePresenceWatcher>();

    fake_driver_ = std::make_unique<FakeAudioDriver>(std::move(server_end), std::move(client_end),
                                                     dispatcher());
  }
  void TearDown() override { fake_device_presence_watcher_.reset(); }

 protected:
  // Used when device_ and fake_driver_ are used (which is the vast majority of cases).
  void InitializeDeviceForFakeDriver() { device_ = InitializeDeviceForFakeDriver(fake_driver_); }

  std::shared_ptr<Device> InitializeDeviceForFakeDriver(
      const std::unique_ptr<FakeAudioDriver>& driver) {
    auto device_type = *driver->is_input() ? fuchsia_audio_device::DeviceType::kInput
                                           : fuchsia_audio_device::DeviceType::kOutput;
    auto stream_config_client_end = driver->Enable();
    auto device = Device::Create(
        std::weak_ptr<FakeDevicePresenceWatcher>(fake_device_presence_watcher_), dispatcher(),
        "Device name", device_type,
        fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(std::move(stream_config_client_end)));

    RunLoopUntilIdle();
    EXPECT_FALSE(device->state_ == Device::State::DeviceInitializing);

    return device;
  }

  void FakeDriverDropStreamConfig() { fake_driver_->DropStreamConfig(); }

  fuchsia_audio_device::Info GetDeviceInfo() const { return *device_->info(); }

  static bool HasError(std::shared_ptr<Device> device) {
    return device->state_ == Device::State::Error;
  }
  static bool InInitializedState(std::shared_ptr<Device> device) {
    return device->state_ == Device::State::DeviceInitialized;
  }
  static bool IsControlled(std::shared_ptr<Device> device) { return device->is_controlled_; }

  bool DevicePluggedState(std::shared_ptr<Device> device) {
    return *device->plug_state_->plugged();
  }
  fuchsia_hardware_audio::GainState DeviceGainState(std::shared_ptr<Device> device) {
    return *device->gain_state_;
  }

  bool SetDeviceGain(fuchsia_hardware_audio::GainState new_state) {
    return device_->SetGain(new_state);
  }
  // bool SetDevicePlugState(bool plugged, zx::time plug_time){return device_->}

  bool SetControl(std::shared_ptr<Device> device) { return device->SetControl(); }
  bool DropControl(std::shared_ptr<Device> device) { return device->DropControl(); }

  std::shared_ptr<Device> device_;
  std::shared_ptr<Clock> device_clock() { return device_->device_clock_; }

  // Receives "OnInitCompletion", "DeviceHasError", "DeviceIsRemoved" notifications from Devices.
  std::shared_ptr<FakeDevicePresenceWatcher> fake_device_presence_watcher_;

  // While |device_| is the object under test, this object simulates the channel messages that
  // normally come from the actual driver instance.
  std::unique_ptr<FakeAudioDriver> fake_driver_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_UNITTEST_H_
