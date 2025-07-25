// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/engine-listener-banjo-adapter.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>
#include <lib/sync/completion.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/coordinator/testing/mock-engine-listener.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"
#include "src/graphics/display/lib/edid-values/edid-values.h"

namespace display_coordinator {

class EngineListenerBanjoAdapterTest : public ::testing::Test {
 public:
  void SetUp() override {
    adapter_ =
        std::make_unique<EngineListenerBanjoAdapter>(&mock_engine_listener_, dispatcher_->borrow());
  }

  void TearDown() override {
    adapter_.reset();
    driver_runtime_.RunUntilIdle();
    mock_engine_listener_.CheckAllCallsReplayed();
  }

  ddk::DisplayEngineListenerProtocolClient CreateEngineListenerClient() {
    display_engine_listener_protocol_t protocol = adapter_->GetProtocol();
    ddk::DisplayEngineListenerProtocolClient client(&protocol);
    ZX_ASSERT(client.is_valid());
    return client;
  }

 protected:
  fdf_testing::ScopedGlobalLogger logger_;
  fdf_testing::DriverRuntime driver_runtime_;
  fdf::UnownedSynchronizedDispatcher dispatcher_{driver_runtime_.StartBackgroundDispatcher()};

  testing::MockEngineListener mock_engine_listener_;
  std::unique_ptr<EngineListenerBanjoAdapter> adapter_;
};

TEST_F(EngineListenerBanjoAdapterTest, OnDisplayAdded) {
  constexpr display::DisplayId kDisplayId(1);
  constexpr fuchsia_images2_pixel_format_enum_value_t kPixelFormats[] = {
      static_cast<fuchsia_images2_pixel_format_enum_value_t>(
          fuchsia_images2::PixelFormat::kR8G8B8A8),
      static_cast<fuchsia_images2_pixel_format_enum_value_t>(
          fuchsia_images2::PixelFormat::kB8G8R8A8),
  };
  constexpr display_mode_t kPreferredModes[] = {
      {
          .pixel_clock_hz = 0x1f'1f'1f'1f'1f,
          .h_addressable = 0x0f'0f,
          .h_front_porch = 0x0a'0a,
          .h_sync_pulse = 0x01'01,
          .h_blanking = 0x0d'0d,
          .v_addressable = 0x0b'0b,
          .v_front_porch = 0x03'03,
          .v_sync_pulse = 0x04'04,
          .v_blanking = 0x0c'0c,
          .flags = MODE_FLAG_INTERLACED | MODE_FLAG_HSYNC_POSITIVE | MODE_FLAG_ALTERNATING_VBLANK,
      },
  };
  constexpr std::span<const uint8_t> kEdidBytes = edid::kDellP2719hEdid;

  const raw_display_info_t valid_banjo_display_info = {
      .display_id = kDisplayId.ToBanjo(),
      .preferred_modes_list = kPreferredModes,
      .preferred_modes_count = std::size(kPreferredModes),
      .edid_bytes_list = kEdidBytes.data(),
      .edid_bytes_count = kEdidBytes.size(),
      .pixel_formats_list = kPixelFormats,
      .pixel_formats_count = std::size(kPixelFormats),
  };

  libsync::Completion completion;
  mock_engine_listener_.ExpectOnDisplayAdded([&](std::unique_ptr<AddedDisplayInfo> info) {
    ASSERT_EQ(fdf::Dispatcher::GetCurrent()->get(), dispatcher_->get());

    EXPECT_EQ(info->display_id, kDisplayId);
    EXPECT_THAT(info->pixel_formats, ::testing::ElementsAre(display::PixelFormat::kR8G8B8A8,
                                                            display::PixelFormat::kB8G8R8A8));

    ASSERT_EQ(info->banjo_preferred_modes.size(), 1u);
    const display_mode_t& banjo_preferred_mode = info->banjo_preferred_modes[0];

    EXPECT_EQ(banjo_preferred_mode.pixel_clock_hz, 0x1f'1f'1f'1f'1f);
    EXPECT_EQ(banjo_preferred_mode.h_addressable, 0x0f'0fu);
    EXPECT_EQ(banjo_preferred_mode.h_front_porch, 0x0a'0au);
    EXPECT_EQ(banjo_preferred_mode.h_sync_pulse, 0x01'01u);
    EXPECT_EQ(banjo_preferred_mode.h_blanking, 0x0d'0du);
    EXPECT_EQ(banjo_preferred_mode.v_addressable, 0x0b'0bu);
    EXPECT_EQ(banjo_preferred_mode.v_front_porch, 0x03'03u);
    EXPECT_EQ(banjo_preferred_mode.v_sync_pulse, 0x04'04u);
    EXPECT_EQ(banjo_preferred_mode.v_blanking, 0x0c'0cu);
    EXPECT_EQ(banjo_preferred_mode.flags,
              MODE_FLAG_INTERLACED | MODE_FLAG_HSYNC_POSITIVE | MODE_FLAG_ALTERNATING_VBLANK);

    EXPECT_THAT(info->edid_bytes, ::testing::ElementsAreArray(kEdidBytes));
    completion.Signal();
  });

  ddk::DisplayEngineListenerProtocolClient client = CreateEngineListenerClient();
  client.OnDisplayAdded(&valid_banjo_display_info);
  completion.Wait();
}

TEST_F(EngineListenerBanjoAdapterTest, OnDisplayRemoved) {
  constexpr display::DisplayId kDisplayId(1);

  libsync::Completion completion;
  mock_engine_listener_.ExpectOnDisplayRemoved([&](display::DisplayId display_id) {
    ASSERT_EQ(fdf::Dispatcher::GetCurrent()->get(), dispatcher_->get());

    EXPECT_EQ(display_id, kDisplayId);
    completion.Signal();
  });

  ddk::DisplayEngineListenerProtocolClient client = CreateEngineListenerClient();
  client.OnDisplayRemoved(kDisplayId.ToBanjo());
  completion.Wait();
}

TEST_F(EngineListenerBanjoAdapterTest, OnDisplayVsync) {
  constexpr display::DisplayId kDisplayId(1);
  const zx::time_monotonic kTimestamp = zx::clock::get_monotonic();
  constexpr config_stamp_t kConfigStamp = {.value = 123};

  libsync::Completion completion;
  mock_engine_listener_.ExpectOnDisplayVsync([&](display::DisplayId display_id,
                                                 zx::time_monotonic timestamp,
                                                 display::DriverConfigStamp driver_config_stamp) {
    ASSERT_EQ(fdf::Dispatcher::GetCurrent()->get(), dispatcher_->get());

    EXPECT_EQ(display_id, kDisplayId);
    EXPECT_EQ(timestamp, kTimestamp);
    EXPECT_EQ(driver_config_stamp.value(), kConfigStamp.value);
    completion.Signal();
  });

  ddk::DisplayEngineListenerProtocolClient client = CreateEngineListenerClient();
  client.OnDisplayVsync(kDisplayId.ToBanjo(), kTimestamp.get(), &kConfigStamp);
  completion.Wait();
}

TEST_F(EngineListenerBanjoAdapterTest, OnCaptureComplete) {
  libsync::Completion completion;
  mock_engine_listener_.ExpectOnCaptureComplete([&] {
    ASSERT_EQ(fdf::Dispatcher::GetCurrent()->get(), dispatcher_->get());

    completion.Signal();
  });

  ddk::DisplayEngineListenerProtocolClient client = CreateEngineListenerClient();
  client.OnCaptureComplete();
  completion.Wait();
}

}  // namespace display_coordinator
