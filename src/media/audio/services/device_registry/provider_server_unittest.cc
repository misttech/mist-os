// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/markers.h>

#include <gtest/gtest.h>

#include "lib/zx/time.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"

namespace media_audio {
namespace {

class ProviderServerTest : public AudioDeviceRegistryServerTestBase {};

TEST_F(ProviderServerTest, AddDeviceThatOutlivesProvider) {
  auto provider_wrapper = std::make_unique<TestServerAndNaturalAsyncClient<ProviderServer>>(
      test_loop(), server_thread_, adr_service_);
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  auto stream_config_client_end =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());

  auto received_callback = false;
  provider_wrapper->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .stream_config_client = std::move(stream_config_client_end),
      }})
      .Then([&received_callback](fidl::Result<fuchsia_audio_device::Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  provider_wrapper.reset();
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

}  // namespace
}  // namespace media_audio
