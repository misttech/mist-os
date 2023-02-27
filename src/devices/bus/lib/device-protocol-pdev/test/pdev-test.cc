// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.device/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/device-protocol/pdev.h>

#include <zxtest/zxtest.h>

#include "zircon/errors.h"

constexpr uint32_t kVid = 1;
constexpr uint32_t kPid = 1;

namespace fhpd = fuchsia_hardware_platform_device;

TEST(PDevTest, GetInterrupt) {
  constexpr zx_handle_t kFakeHandle = 3;
  pdev_protocol_ops_t fake_ops{
      .get_interrupt =
          [](void* ctx, uint32_t index, uint32_t flags, zx_handle_t* out_irq) {
            *out_irq = kFakeHandle;
            return ZX_OK;
          },
  };

  pdev_protocol_t fake_proto{
      .ops = &fake_ops,
      .ctx = nullptr,
  };

  ddk::PDev pdev(&fake_proto);
  zx::interrupt out;
  ASSERT_OK(pdev.GetInterrupt(0, 0, &out));
  ASSERT_EQ(out.get(), kFakeHandle);
}

class DeviceServer : public fidl::testing::WireTestBase<fuchsia_hardware_platform_device::Device> {
 public:
  zx_status_t Connect(fidl::ServerEnd<fhpd::Device> request) {
    if (binding_.has_value()) {
      return ZX_ERR_ALREADY_BOUND;
    }
    binding_.emplace(async_get_default_dispatcher(), std::move(request), this,
                     fidl::kIgnoreBindingClosure);
    return ZX_OK;
  }

 private:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override {
    fidl::Arena arena;
    completer.ReplySuccess(fuchsia_hardware_platform_device::wire::BoardInfo::Builder(arena)
                               .vid(kVid)
                               .pid(kPid)
                               .Build());
  }
  std::optional<fidl::ServerBinding<fuchsia_hardware_platform_device::Device>> binding_;
};

TEST(PDevFidlTest, GetBoardInfo) {
  async::Loop server_loop{&kAsyncLoopConfigNoAttachToCurrentThread};
  server_loop.StartThread("fidl-thread");

  async_patterns::TestDispatcherBound<DeviceServer> server{server_loop.dispatcher(), std::in_place};

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_platform_device::Device>();
  ASSERT_OK(endpoints);
  ASSERT_OK(server.SyncCall(&DeviceServer::Connect, std::move(endpoints->server)));

  ddk::PDevFidl pdev{std::move(endpoints->client)};
  pdev_board_info_t board_info;
  ASSERT_OK(pdev.GetBoardInfo(&board_info));
  ASSERT_EQ(kPid, board_info.pid);
  ASSERT_EQ(kVid, board_info.vid);
}
