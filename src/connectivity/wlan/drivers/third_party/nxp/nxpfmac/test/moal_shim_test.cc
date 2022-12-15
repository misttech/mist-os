// Copyright (c) 2022 The Fuchsia Authors
//
// Permission to use, copy, modify, and/or distribute this software for any purpose with or without
// fee is hereby granted, provided that the above copyright notice and this permission notice
// appear in all copies.
//
// THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS
// SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
// AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
// WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT,
// NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE
// OF THIS SOFTWARE.

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/moal_shim.h"

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/device.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/device_context.h"
#include "src/connectivity/wlan/drivers/third_party/nxp/nxpfmac/event_handler.h"

namespace {

struct DispatcherDevice : public wlan::nxpfmac::Device {
  DispatcherDevice() : Device(nullptr) {}
  zx_status_t Init() { return loop_.StartThread(); }
  async_dispatcher_t* GetDispatcher() override { return loop_.dispatcher(); }
  zx_status_t Init(mlan_device* mlan_dev, wlan::nxpfmac::BusInterface** out_bus) override {
    return ZX_OK;
  }
  zx_status_t LoadFirmware(const char* path, zx::vmo* out_fw, size_t*) override { return ZX_OK; }
  void Shutdown() override {}

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
};

struct MoalShimTest : public zxtest::Test {
  void SetUp() override {
    ASSERT_OK(device_.Init());
    context_.device_ = &device_;
    context_.event_handler_ = &event_handler_;
    wlan::nxpfmac::populate_callbacks(&mlan_device_);
  }
  mlan_device mlan_device_;
  DispatcherDevice device_;
  wlan::nxpfmac::EventHandler event_handler_;
  wlan::nxpfmac::DeviceContext context_{};
};

TEST_F(MoalShimTest, RecvEvent) {
  const auto test_thread = std::this_thread::get_id();
  constexpr mlan_event_id kEventId = MLAN_EVENT_ID_DRV_CONNECTED;
  sync_completion_t event_received;
  mlan_event event{.event_id = kEventId};
  auto event_registration =
      event_handler_.RegisterForEvent(kEventId, [&, event_ptr = &event](pmlan_event event) {
        // Event should be processed on a separate thread from the one on which the event was
        // triggered.
        EXPECT_NE(test_thread, std::this_thread::get_id());
        EXPECT_EQ(kEventId, event->event_id);
        EXPECT_BYTES_EQ(event_ptr, event, sizeof(*event));
        sync_completion_signal(&event_received);
      });
  ASSERT_EQ(MLAN_STATUS_SUCCESS, mlan_device_.callbacks.moal_recv_event(&context_, &event));
  sync_completion_wait(&event_received, ZX_TIME_INFINITE);
}

}  // namespace
