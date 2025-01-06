// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire_test_base.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/vmo.h>

#include <cstdint>
#include <memory>

#include <gtest/gtest.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-meson/g12b-clk.h>
#include <soc/aml-s905d2/s905d2-hiu-regs.h>

#include "src/devices/lib/mmio/test-helper.h"
#include "vim3_clk.h"

namespace vim3_clock {

class FakePDev : public fidl::testing::WireTestBase<fuchsia_hardware_platform_device::Device> {
 public:
  FakePDev()
      : hui_mmio_(fdf_testing::CreateMmioBuffer(A311D_HIU_LENGTH, ZX_CACHE_POLICY_UNCACHED_DEVICE)),
        dos_mmio_(
            fdf_testing::CreateMmioBuffer(A311D_DOS_LENGTH, ZX_CACHE_POLICY_UNCACHED_DEVICE)) {
    hui_mmio_.Write32(HHI_PLL_LOCK, HHI_GP0_PLL_CNTL0);
  }

  fuchsia_hardware_platform_device::Service::InstanceHandler GetInstanceHandler(
      async_dispatcher_t* dispatcher) {
    return fuchsia_hardware_platform_device::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure),
    });
  }

  void ClearHiu() { ClearMmioBuffer(hui_mmio_); }
  void ClearDos() { ClearMmioBuffer(dos_mmio_); }

  bool IsHiuDirty() const { return IsMmioBufferDirty(hui_mmio_); }
  bool IsDosDirty() const { return IsMmioBufferDirty(dos_mmio_); }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {}

  void GetMmioById(GetMmioByIdRequestView request, GetMmioByIdCompleter::Sync& completer) override {
    zx::vmo vmo;
    zx_status_t st;
    size_t sz;
    switch (request->index) {
      case kHiuMmioIndex:
        st = hui_mmio_.get_vmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo);
        sz = A311D_HIU_LENGTH;
        break;
      case kDosMmioIndex:
        st = dos_mmio_.get_vmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo);
        sz = A311D_DOS_LENGTH;
        break;
      default:
        completer.ReplyError(ZX_ERR_INVALID_ARGS);
        return;
    }

    if (st != ZX_OK) {
      completer.ReplyError(st);
      return;
    }

    fidl::Arena arena;
    auto result = fuchsia_hardware_platform_device::wire::Mmio::Builder(arena)
                      .size(sz)
                      .offset(0)
                      .vmo(std::move(vmo))
                      .Build();

    completer.ReplySuccess(std::move(result));
  }

  void GetMetadata(GetMetadataRequestView request, GetMetadataCompleter::Sync& completer) override {
    auto metadata_id = request->id.get();

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
    if (metadata_id == fuchsia_hardware_clockimpl::ClockIdsMetadata::kSerializableName) {
      fuchsia_hardware_clockimpl::ClockIdsMetadata metadata{{.clock_ids{}}};
      fit::result encoded_metadata = fidl::Persist(metadata);
      if (encoded_metadata.is_error()) {
        completer.ReplyError(encoded_metadata.error_value().status());
        return;
      }
      completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(encoded_metadata.value()));
      return;
    }
#endif

    if (metadata_id == fuchsia_hardware_clockimpl::InitMetadata::kSerializableName) {
      fuchsia_hardware_clockimpl::InitMetadata metadata{{.steps{}}};
      fit::result encoded_metadata = fidl::Persist(metadata);
      if (encoded_metadata.is_error()) {
        completer.ReplyError(encoded_metadata.error_value().status());
        return;
      }
      completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(encoded_metadata.value()));
      return;
    }

    completer.ReplyError(ZX_ERR_NOT_FOUND);
  }

 private:
  static void ClearMmioBuffer(fdf::MmioBuffer& buffer) {
    const size_t sz = buffer.get_size();
    auto empty = std::make_unique<uint8_t[]>(sz);
    memset(empty.get(), 0, sz);
    buffer.WriteBuffer(0, empty.get(), sz);
  }

  static bool IsMmioBufferDirty(const fdf::MmioBuffer& buffer) {
    const size_t sz = buffer.get_size();
    auto contents = std::make_unique<uint8_t[]>(sz);
    buffer.ReadBuffer(0, contents.get(), sz);

    for (size_t i = 0; i < sz; i++) {
      if (contents.get()[i] != 0x0) {
        return true;
      }
    }
    return false;
  }

  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> binding_group_;

  fdf::MmioBuffer hui_mmio_;
  fdf::MmioBuffer dos_mmio_;
};

class TestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    async_dispatcher_t* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
    auto result = to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
        pdev_.GetInstanceHandler(dispatcher));
    EXPECT_EQ(ZX_OK, result.status_value());

    return zx::ok();
  }

  FakePDev& PDev() { return pdev_; }

 private:
  FakePDev pdev_;
};

class FixtureConfig final {
 public:
  using DriverType = Vim3Clock;
  using EnvironmentType = TestEnvironment;
};

class DriverTest : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result<> result = driver_test().StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
    zx::result device_result = driver_test().Connect<fuchsia_hardware_clockimpl::Service::Device>();
    ASSERT_EQ(device_result.status_value(), ZX_OK);
    client_.Bind(std::move(device_result.value()));
  }

  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  fdf_testing::BackgroundDriverTest<FixtureConfig>& driver_test() { return driver_test_; }
  fdf_testing::BackgroundDriverTest<FixtureConfig> driver_test_;

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

TEST_F(DriverTest, ClkTestHiuRegRegion) {
  /// Make sure that HIU clocks are actually touching the HIU registers.
  fdf::Arena arena('TEST');

  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.PDev().ClearDos();
    env.PDev().ClearHiu();
  });

  auto enable_result = client_.buffer(arena)->Enable(g12b_clk::G12B_CLK_AUDIO);
  ASSERT_TRUE(enable_result.ok());

  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    ASSERT_TRUE(env.PDev().IsHiuDirty());
    ASSERT_FALSE(env.PDev().IsDosDirty());
  });
}

TEST_F(DriverTest, ClkTestDosRegRegion) {
  /// Make sure that DOS clocks are actually touching the DOS registers.
  fdf::Arena arena('TEST');

  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    env.PDev().ClearDos();
    env.PDev().ClearHiu();
  });

  auto enable_result = client_.buffer(arena)->Enable(g12b_clk::G12B_CLK_DOS_GCLK_VDEC);
  ASSERT_TRUE(enable_result.ok());

  driver_test().RunInEnvironmentTypeContext([](TestEnvironment& env) {
    ASSERT_FALSE(env.PDev().IsHiuDirty());
    ASSERT_TRUE(env.PDev().IsDosDirty());
  });
}

}  // namespace vim3_clock
