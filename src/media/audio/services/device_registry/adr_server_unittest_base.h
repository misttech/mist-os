// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_ADR_SERVER_UNITTEST_BASE_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_ADR_SERVER_UNITTEST_BASE_H_

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.mediastreams/cpp/common_types.h>
#include <fidl/fuchsia.mediastreams/cpp/natural_types.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>

#include <memory>
#include <optional>
#include <string_view>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/media/audio/services/common/fidl_thread.h"
#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/audio_device_registry.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/provider_server.h"
#include "src/media/audio/services/device_registry/registry_server.h"
#include "src/media/audio/services/device_registry/testing/fake_audio_driver.h"

namespace media_audio {

class AudioDeviceRegistryServerTestBase : public gtest::TestLoopFixture {
 protected:
  // Create a FakeAudioDriver that can mock a real device that has been detected, using default
  // settings. From here, the fake driver can be customized before calling EnableFakeDriver().
  std::unique_ptr<FakeAudioDriver> CreateFakeDriver() {
    EXPECT_EQ(dispatcher(), test_loop().dispatcher());
    zx::channel server_end, client_end;
    EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
    return std::make_unique<FakeAudioDriver>(std::move(server_end), std::move(client_end),
                                             dispatcher());
  }

  // Device
  // Create a Device object (backed by a fake driver); insert it to ADR as if it had been detected.
  // Through the stream_config connection, this will communicate with the fake driver.
  void AddDeviceForDetection(
      std::string_view name, fuchsia_audio_device::DeviceType device_type,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig> stream_config_client_end) {
    adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), name, device_type,
                                           std::move(stream_config_client_end)));
  }

  // Provider
  std::unique_ptr<TestServerAndNaturalAsyncClient<ProviderServer>> CreateProviderServer() {
    return std::make_unique<TestServerAndNaturalAsyncClient<ProviderServer>>(
        test_loop(), server_thread_, adr_service_);
  }

  std::shared_ptr<FidlThread> server_thread_ =
      FidlThread::CreateFromCurrentThread("test_server_thread", dispatcher());

  std::shared_ptr<media_audio::AudioDeviceRegistry> adr_service_ =
      std::make_shared<media_audio::AudioDeviceRegistry>(server_thread_);
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_ADR_SERVER_UNITTEST_BASE_H_
