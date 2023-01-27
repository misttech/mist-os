// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/audio_device_registry.h"

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/testing/fake_audio_driver.h"

namespace media_audio {
namespace {

class AudioDeviceRegistryTest : public AudioDeviceRegistryServerTestBase {};

TEST_F(AudioDeviceRegistryTest, DeviceInitialization) {
  auto fake_driver = CreateFakeDriver();
  auto stream_config_client =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());
  AddDeviceForDetection("test output", fuchsia_audio_device::DeviceType::kOutput,
                        std::move(stream_config_client));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

// TODO(fxbug.dev/117826): StreamConfigDisconnect test, after added and healthy

TEST_F(AudioDeviceRegistryTest, FindDeviceByTokenId) {
  auto fake_driver = CreateFakeDriver();
  auto stream_config_client =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());
  AddDeviceForDetection("test output", fuchsia_audio_device::DeviceType::kOutput,
                        std::move(stream_config_client));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  auto token_id = adr_service_->devices().begin()->get()->token_id();

  EXPECT_EQ(adr_service_->FindDeviceByTokenId(token_id).first,
            AudioDeviceRegistry::DevicePresence::Active);
}

}  // namespace
}  // namespace media_audio
