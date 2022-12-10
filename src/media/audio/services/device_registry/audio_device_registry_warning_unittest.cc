// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>

#include <gtest/gtest.h>

#include "fidl/fuchsia.audio.device/cpp/common_types.h"
#include "fidl/fuchsia.hardware.audio/cpp/natural_types.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/audio_device_registry.h"
#include "src/media/audio/services/device_registry/testing/fake_audio_driver.h"

namespace media_audio {

class AudioDeviceRegistryServerTest : public AudioDeviceRegistryServerTestBase {};

TEST_F(AudioDeviceRegistryServerTest, UnhealthyDevice) {
  auto fake_driver = CreateFakeDriver();
  fake_driver->set_health_state(false);
  auto stream_config_client =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());
  AddDeviceForDetection("test output", fuchsia_audio_device::DeviceType::kOutput,
                        std::move(stream_config_client));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 1u);
}

// TODO: StreamConfigDisconnect test, after added and healthy

}  // namespace media_audio
