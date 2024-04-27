// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/fixtures/gtest_fixture.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/mmio/mmio-buffer.h>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <gtest/gtest.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-meson/g12b-clk.h>
#include <soc/aml-s905d2/s905d2-hiu-regs.h>
#include <src/devices/bus/testing/fake-pdev/fake-pdev.h>

#include "../vim3_clk.h"
#include "fidl/fuchsia.hardware.clockimpl/cpp/markers.h"
#include "lib/component/incoming/cpp/constants.h"
#include "lib/zx/handle.h"
#include "lib/zx/resource.h"
#include "lib/zx/vmo.h"
#include "src/devices/lib/mmio/test-helper.h"

namespace vim3_clock {

constexpr size_t kRegisterCount = A311D_HIU_LENGTH / sizeof(uint32_t);
class FakeHiuMmio {
 public:
  FakeHiuMmio() : mmio_(sizeof(uint32_t), kRegisterCount) {
    mmio_[HHI_GP0_PLL_CNTL0].SetReadCallback([]() { return HHI_PLL_LOCK; });
  }

  fdf::MmioBuffer mmio() { return mmio_.GetMmioBuffer(); }

 private:
  ddk_fake::FakeMmioRegRegion mmio_;
};

class TestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    fake_pdev::FakePDevFidl::Config config;

    config.mmios[kHiuMmioIndex] = hiu_mmio_.mmio();
    config.mmios[kDosMmioIndex] =
        fdf_testing::CreateMmioBuffer(A311D_DOS_LENGTH, ZX_CACHE_POLICY_UNCACHED_DEVICE);

    pdev_.SetConfig(std::move(config));

    async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
    auto result = to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
        pdev_.GetInstanceHandler(dispatcher));
    EXPECT_EQ(ZX_OK, result.status_value());

    return zx::ok();
  }

 private:
  fake_pdev::FakePDevFidl pdev_;
  FakeHiuMmio hiu_mmio_;
};

class FixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = false;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = Vim3Clock;
  using EnvironmentType = TestEnvironment;
};

class DriverTest : public fdf_testing::DriverTestFixture<FixtureConfig> {
 protected:
  void SetUp() override {
    zx::result device_result = Connect<fuchsia_hardware_clockimpl::Service::Device>();
    ASSERT_EQ(device_result.status_value(), ZX_OK);
    client_.Bind(std::move(device_result.value()));
  }

  fdf::WireSyncClient<fuchsia_hardware_clockimpl::ClockImpl> client_;
};

TEST_F(DriverTest, EnableDisableMesonGate) {
  fdf::Arena arena('TEST');

  // Pick an arbitrary Meson Gate to Enable and Disable. Make sure it works.
  auto enable_result = client_.buffer(arena)->Enable(g12b_clk::G12B_CLK_AUDIO);
  ASSERT_TRUE(enable_result.ok());
  ASSERT_TRUE(enable_result->is_ok());

  auto disable_result = client_.buffer(arena)->Disable(g12b_clk::G12B_CLK_AUDIO);
  ASSERT_TRUE(disable_result.ok());
  ASSERT_TRUE(disable_result->is_ok());
}

TEST_F(DriverTest, EnableDisableMesonPll) {
  fdf::Arena arena('TEST');

  // Pick an arbitrary Meson Gate to Disable. Make sure it works.
  auto disable_result = client_.buffer(arena)->Disable(g12b_clk::CLK_PCIE_PLL);
  ASSERT_TRUE(disable_result.ok());
  ASSERT_TRUE(disable_result->is_ok());
}

TEST_F(DriverTest, EnableDisableInvalid) {
  fdf::Arena arena('TEST');

  {
    // CPU Clocks don't support Enable
    auto result = client_.buffer(arena)->Enable(g12b_clk::CLK_SYS_CPU_BIG_CLK);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
  }

  {
    // CPU Clocks don't support Disable
    auto result = client_.buffer(arena)->Disable(g12b_clk::CLK_SYS_CPU_BIG_CLK);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
  }

  {
    // Invent a new Meson Clock and try to enable it
    constexpr uint32_t kFakeMesonClockID =
        AmlClkId(0xBEEF, aml_clk_common::aml_clk_type::kMesonGate);
    auto result = client_.buffer(arena)->Enable(kFakeMesonClockID);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
  }

  {
    // Invent a new Meson Clock and try to disable it
    constexpr uint32_t kFakeMesonClockID =
        AmlClkId(0xBEEF, aml_clk_common::aml_clk_type::kMesonGate);
    auto result = client_.buffer(arena)->Disable(kFakeMesonClockID);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
  }
}

TEST_F(DriverTest, ClkMuxUnsupported) {
  // These are placeholder tests just to exercise the mux interfaces even though they are
  // unsupporeted.
  fdf::Arena arena('TEST');
  {
    auto result = client_.buffer(arena)->SetInput(0, 0);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
    ASSERT_EQ(result->error_value(), ZX_ERR_NOT_SUPPORTED);
  }

  {
    auto result = client_.buffer(arena)->GetNumInputs(0);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
    ASSERT_EQ(result->error_value(), ZX_ERR_NOT_SUPPORTED);
  }

  {
    auto result = client_.buffer(arena)->GetInput(0);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
    ASSERT_EQ(result->error_value(), ZX_ERR_NOT_SUPPORTED);
  }
}

TEST_F(DriverTest, ClkEnablePll) {
  fdf::Arena arena('TEST');

  {
    auto result = client_.buffer(arena)->Enable(g12b_clk::CLK_GP0_PLL);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  }
}

}  // namespace vim3_clock
