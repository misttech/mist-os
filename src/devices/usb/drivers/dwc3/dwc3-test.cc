// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/dwc3/dwc3.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <gtest/gtest.h>

#include "lib/driver/fake-platform-device/cpp/fake-pdev.h"
#include "lib/driver/testing/cpp/driver_test.h"
#include "src/devices/usb/drivers/dwc3/dwc3-regs.h"

namespace dwc3 {

namespace fpdev = fuchsia_hardware_platform_device;

class Environment : public fdf_testing::Environment {
 public:
  Environment() {
    auto config = fdf_fake::FakePDev::Config{};
    config.mmios[0] = reg_region_.GetMmioBuffer();
    config.use_fake_bti = true;
    config.use_fake_irq = true;

    pdev_.SetConfig(std::move(config));
  }

  zx::result<> Serve(fdf::OutgoingDirectory& directory) override {
    auto result = directory.AddService<fpdev::Service>(
        pdev_.GetInstanceHandler(fdf::Dispatcher::GetCurrent()->async_dispatcher()), "pdev");
    EXPECT_TRUE(result.is_ok());

    device_server_.Initialize(component::kDefaultInstance);
    return zx::make_result(
        device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &directory));
  }

  ddk_fake::FakeMmioRegRegion& reg_region() { return reg_region_; }

 private:
  static constexpr size_t kRegSize = sizeof(uint32_t);
  static constexpr size_t kMmioRegionSize = 64 << 10;
  static constexpr size_t kRegCount = kMmioRegionSize / kRegSize;

  fdf_fake::FakePDev pdev_;
  compat::DeviceServer device_server_;
  ddk_fake::FakeMmioRegRegion reg_region_{kRegSize, kRegCount};
};

class Config final {
 public:
  using DriverType = Dwc3;
  using EnvironmentType = Environment;
};

// Test is templated on a parameter which, if true, will have the harness start and stop the driver.
// Otherwise, it is the individual test(s) responsibility to start and stop the driver.
template <bool manage_lifetime>
class TestFixture : public testing::Test {
 public:
  void SetUp() override {
    dut_.RunInEnvironmentTypeContext([&](Environment& env) {
      auto& hwparams3 = env.reg_region()[GHWPARAMS3::Get().addr()];
      auto& ver_reg = env.reg_region()[USB31_VER_NUMBER::Get().addr()];
      auto& dctl_reg = env.reg_region()[DCTL::Get().addr()];

      hwparams3.SetReadCallback([this]() -> uint64_t { return Read_GHWPARAMS3(); });
      ver_reg.SetReadCallback([this]() -> uint64_t { return Read_USB31_VER_NUMBER(); });
      dctl_reg.SetReadCallback([this]() -> uint64_t { return Read_DCTL(); });
      dctl_reg.SetWriteCallback([this](uint64_t val) { return Write_DCTL(val); });
    });

    if (manage_lifetime) {
      EXPECT_EQ(ZX_OK, dut_.StartDriver().status_value());
    }
  }

  void TearDown() override {
    if (manage_lifetime) {
      EXPECT_EQ(ZX_OK, dut_.StopDriver().status_value());
    }
  }

 protected:
  // Section 1.3.22 of the DWC3 Programmer's guide
  //
  // DWC_USB31_CACHE_TOTAL_XFER_RESOURCES : 32
  // DWC_USB31_NUM_IN_EPS                 : 16
  // DWC_USB31_NUM_EPS                    : 32
  // DWC_USB31_VENDOR_CTL_INTERFACE       : 0
  // DWC_USB31_HSPHY_DWIDTH               : 2
  // DWC_USB31_HSPHY_INTERFACE            : 1
  // DWC_USB31_SSPHY_INTERFACE            : 2
  uint64_t Read_GHWPARAMS3() { return 0x10420086; }

  // Section 1.3.45 of the DWC3 Programmer's guide
  uint64_t Read_USB31_VER_NUMBER() { return 0x31363061; }  // 1.60a

  // Section 1.4.2 of the DWC3 Programmer's guide
  uint64_t Read_DCTL() { return dctl_val_; }
  void Write_DCTL(uint64_t val) {
    constexpr uint32_t kUnwriteableMask =
        (1 << 29) | (1 << 17) | (1 << 16) | (1 << 15) | (1 << 14) | (1 << 13) | (1 << 0);
    ZX_DEBUG_ASSERT(val <= std::numeric_limits<uint32_t>::max());
    dctl_val_ = static_cast<uint32_t>(val & ~kUnwriteableMask);

    // Immediately clear the soft reset bit if we are not testing the soft reset
    // timeout behavior.
    if (!stuck_reset_test_) {
      dctl_val_ = DCTL::Get().FromValue(dctl_val_).set_CSFTRST(0).reg_value();
    }
  }

  uint32_t dctl_val_ = DCTL::Get().FromValue(0).set_LPM_NYET_thres(0xF).reg_value();
  bool stuck_reset_test_{false};

  fdf_testing::ForegroundDriverTest<Config> dut_;
};

using ManagedTestFixture = TestFixture<true>;
using UnmanagedTestFixture = TestFixture<false>;

TEST_F(ManagedTestFixture, Dfv2Lifecycle) {
  dut_.RunInNodeContext(
      [&](fdf_testing::TestNode& node) { EXPECT_EQ(1UL, node.children().size()); });
}

TEST_F(UnmanagedTestFixture, Dfv2HwResetTimeout) {
  stuck_reset_test_ = true;
  EXPECT_EQ(ZX_ERR_TIMED_OUT, dut_.StartDriver().status_value());

  dut_.RunInNodeContext(
      [&](fdf_testing::TestNode& node) { EXPECT_EQ(0UL, node.children().size()); });

  // The dfv2 driver did not start, nothing to stop.
}

}  // namespace dwc3
