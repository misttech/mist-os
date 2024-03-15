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

    fake_hci_server_.emplace(hci_.NewRequest(), dispatcher());

    fake_vendor_server_.emplace(vendor_.NewRequest(), dispatcher());
  }

  void TearDown() override {
    if (host_) {
      host_->ShutDown();
    }
    host_ = nullptr;
    TestingBase::TearDown();
  }

  fuchsia::hardware::bluetooth::HciHandle hci() { return std::move(hci_); }

  fuchsia::hardware::bluetooth::VendorHandle vendor() { return std::move(vendor_); }

 protected:
  BtHostComponent* host() const { return host_.get(); }

  void DestroyHost() { host_ = nullptr; }

 private:
  std::unique_ptr<BtHostComponent> host_;

  fuchsia::hardware::bluetooth::HciHandle hci_;

  fuchsia::hardware::bluetooth::VendorHandle vendor_;

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
