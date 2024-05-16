// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "host.h"

#include <lib/fidl/cpp/binding.h>

#include <string>

#include "src/connectivity/bluetooth/core/bt-host/fidl/fake_hci_server.h"
#include "src/connectivity/bluetooth/core/bt-host/fidl/fake_vendor_server.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace bthost::testing {

using TestingBase = ::gtest::TestLoopFixture;

const std::string DEFAULT_DEV_PATH = "/dev/class/bt-hci/000";
class HostComponentTest : public TestingBase {
 public:
  HostComponentTest() = default;
  ~HostComponentTest() override = default;

  void SetUp() override {
    host_ = BtHostComponent::CreateForTesting(dispatcher(), DEFAULT_DEV_PATH);

    auto [vendor_client_end, vendor_server_end] =
        fidl::Endpoints<fuchsia_hardware_bluetooth::Vendor>::Create();

    auto [hci_client_end, hci_server_end] =
        fidl::Endpoints<fuchsia_hardware_bluetooth::Hci>::Create();

    vendor_ = std::move(vendor_client_end);
    hci_ = std::move(hci_client_end);

    fake_hci_server_.emplace(std::move(hci_server_end), dispatcher());
    fake_vendor_server_.emplace(std::move(vendor_server_end), dispatcher());
  }

  void TearDown() override {
    if (host_) {
      host_->ShutDown();
    }
    host_ = nullptr;
    TestingBase::TearDown();
  }

  fidl::ClientEnd<fuchsia_hardware_bluetooth::Hci> hci() { return std::move(hci_); }

  fidl::ClientEnd<fuchsia_hardware_bluetooth::Vendor> vendor() { return std::move(vendor_); }

 protected:
  BtHostComponent* host() const { return host_.get(); }

  void DestroyHost() { host_ = nullptr; }

 private:
  std::unique_ptr<BtHostComponent> host_;

  fidl::ClientEnd<fuchsia_hardware_bluetooth::Hci> hci_;

  fidl::ClientEnd<fuchsia_hardware_bluetooth::Vendor> vendor_;

  std::optional<bt::fidl::testing::FakeHciServer> fake_hci_server_;

  std::optional<bt::fidl::testing::FakeVendorServer> fake_vendor_server_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(HostComponentTest);
};

TEST_F(HostComponentTest, InitializeFailsWhenCommandTimesOut) {
  std::optional<bool> init_cb_result;
  bool error_cb_called = false;
  bool init_result = host()->Initialize(
      vendor(),
      [&](bool success) {
        init_cb_result = success;
        if (!success) {
          host()->ShutDown();
        }
      },
      [&]() { error_cb_called = true; });
  EXPECT_EQ(init_result, true);

  constexpr zx::duration kCommandTimeout = zx::sec(15);
  RunLoopFor(kCommandTimeout);
  ASSERT_TRUE(init_cb_result.has_value());
  EXPECT_FALSE(*init_cb_result);
  EXPECT_FALSE(error_cb_called);
}

}  // namespace bthost::testing
