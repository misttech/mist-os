// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_RVICES_DEVICE_REGISTRY_INSPECTOR_UNITTEST_H_
#define SRC_MEDIA_RVICES_DEVICE_REGISTRY_INSPECTOR_UNITTEST_H_

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/reader.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/common/fidl_thread.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/inspector.h"
#include "src/media/audio/services/device_registry/testing/fake_composite.h"

namespace media_audio {

// This provides unittest functions for Inspector and its child classes.
class InspectorTest : public AudioDeviceRegistryServerTestBase {
 protected:
  static inline const fuchsia_audio_device::RingBufferOptions kDefaultRingBufferOptions{{
      .format = fuchsia_audio::Format{{.sample_type = fuchsia_audio::SampleType::kInt16,
                                       .channel_count = 2,
                                       .frames_per_second = 48000}},
      .ring_buffer_min_bytes = 2000,
  }};

  static inspect::Hierarchy GetHierarchy() {
    auto& component_inspector = Inspector::Singleton()->component_inspector();
    auto& inspector = component_inspector->inspector();

    zx::vmo duplicate = inspector.DuplicateVmo();
    if (duplicate.get() == ZX_HANDLE_INVALID) {
      return inspect::Hierarchy();
    }

    auto ret = inspect::ReadFromVmo(duplicate);
    EXPECT_TRUE(ret.is_ok());
    if (ret.is_ok()) {
      return ret.take_value();
    }

    return inspect::Hierarchy();
  }

  std::shared_ptr<FakeComposite> CreateAndAddFakeComposite() {
    auto fake_driver = CreateFakeComposite();
    adr_service()->AddDevice(
        Device::Create(adr_service(), dispatcher(), "Test composite name",
                       fuchsia_audio_device::DeviceType::kComposite,
                       fuchsia_audio_device::DriverClient::WithComposite(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }

  std::shared_ptr<FakeStreamConfig> CreateAndAddFakeStreamConfigOutput() {
    auto fake_driver = CreateFakeStreamConfigOutput();
    adr_service()->AddDevice(Device::Create(
        adr_service(), dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
        fuchsia_audio_device::DriverClient::WithStreamConfig(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }

  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& registry_client) {
    std::optional<TokenId> added_device_id;
    registry_client->WatchDevicesAdded().Then(
        [&added_device_id](
            fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
          ASSERT_TRUE(result.is_ok()) << result.error_value();
          ASSERT_TRUE(result->devices().has_value());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id().has_value());
          added_device_id = result->devices()->at(0).token_id();
        });
    RunLoopUntilIdle();
    return added_device_id;
  }

 private:
  std::shared_ptr<FidlThread> server_thread_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_RVICES_DEVICE_REGISTRY_INSPECTOR_UNITTEST_H_
