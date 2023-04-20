// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-canvas.h"

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/runtime/testing/runtime/dispatcher.h>
#include <lib/driver/testing/cpp/driver_runtime_env.h>
#include <lib/fake-bti/bti.h>

#include <vector>

#include <mock-mmio-reg/mock-mmio-reg.h>
#include <zxtest/zxtest.h>

#include "dmc-regs.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {
constexpr uint32_t kMmioRegSize = sizeof(uint32_t);
constexpr uint32_t kMmioRegCount = (aml_canvas::kDmcCavMaxRegAddr + kMmioRegSize) / kMmioRegSize;
constexpr uint32_t kVmoTestSize = PAGE_SIZE;

constexpr fuchsia_hardware_amlogiccanvas::wire::CanvasInfo test_canvas_info = []() {
  fuchsia_hardware_amlogiccanvas::wire::CanvasInfo ci = {};
  ci.height = 240;
  ci.stride_bytes = 16;
  ci.blkmode = fuchsia_hardware_amlogiccanvas::CanvasBlockMode::kLinear;
  return ci;
}();

constexpr fuchsia_hardware_amlogiccanvas::wire::CanvasInfo invalid_canvas_info = []() {
  fuchsia_hardware_amlogiccanvas::wire::CanvasInfo ci = {};
  ci.height = 240;
  ci.stride_bytes = 15;
  ci.blkmode = fuchsia_hardware_amlogiccanvas::CanvasBlockMode::kLinear;
  return ci;
}();
}  // namespace

namespace aml_canvas {

template <typename T>
ddk_mock::MockMmioReg& GetMockReg(ddk_mock::MockMmioRegRegion& registers) {
  return registers[T::Get().addr()];
}

class AmlCanvasWrap {
 public:
  void Init(fdf::MmioBuffer mmio, zx::bti bti) {
    canvas_ = std::make_unique<AmlCanvas>(fake_parent_.get(), std::move(mmio), std::move(bti));
  }

  void Serve(fidl::ServerEnd<fuchsia_hardware_amlogiccanvas::Device> server_end) {
    binding_.emplace(dispatcher_, std::move(server_end), canvas_.get(),
                     fidl::kIgnoreBindingClosure);
  }

  zx_status_t DdkAdd(const std::string& name) { return canvas_->DdkAdd(name.c_str()); }

  void release() { [[maybe_unused]] auto ptr = canvas_.release(); }

 private:
  async_dispatcher_t* dispatcher_{fdf::Dispatcher::GetCurrent()->async_dispatcher()};
  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();
  std::unique_ptr<AmlCanvas> canvas_;
  std::optional<fidl::ServerBinding<fuchsia_hardware_amlogiccanvas::Device>> binding_;
};

class AmlCanvasTest : public zxtest::Test {
 public:
  AmlCanvasTest() : mock_regs_(ddk_mock::MockMmioRegRegion(kMmioRegSize, kMmioRegCount)) {
    fdf::MmioBuffer mmio(mock_regs_.GetMmioBuffer());

    zx::bti bti;
    EXPECT_OK(fake_bti_create(bti.reset_and_get_address()));

    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_amlogiccanvas::Device>();
    ASSERT_OK(endpoints);

    canvas_.SyncCall(&AmlCanvasWrap::Init, std::move(mmio), std::move(bti));
    canvas_.SyncCall(&AmlCanvasWrap::Serve, std::move(endpoints.value().server));

    canvas_client_.Bind(std::move(endpoints.value().client));
  }

  void TestLifecycle() {
    std::string name = "aml-canvas";
    EXPECT_OK(canvas_.SyncCall(&AmlCanvasWrap::DdkAdd, name));
    canvas_.SyncCall(&AmlCanvasWrap::release);
  }

  zx_status_t CreateNewCanvas() {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(kVmoTestSize, 0, &vmo));

    fidl::WireResult result = canvas_client_->Config(std::move(vmo), 0, test_canvas_info);
    if (!result.ok()) {
      return result.error().status();
    }
    if (result->is_error()) {
      return result->error_value();
    }

    canvas_indices_.push_back(result->value()->canvas_idx);
    return ZX_OK;
  }

  zx_status_t CreateNewCanvasInvalid() {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(kVmoTestSize, 0, &vmo));

    fidl::WireResult result = canvas_client_->Config(std::move(vmo), 0, invalid_canvas_info);
    if (!result.ok()) {
      return result.error().status();
    }
    if (result->is_error()) {
      return result->error_value();
    }

    // We should be returning an error since we are creating an invalid canvas.
    return ZX_ERR_INTERNAL;
  }

  zx_status_t FreeCanvas(uint8_t index) {
    auto it = std::find(canvas_indices_.begin(), canvas_indices_.end(), index);
    if (it != canvas_indices_.end()) {
      canvas_indices_.erase(it);
    }

    fidl::WireResult result = canvas_client_->Free(index);
    if (!result.ok()) {
      return result.error().status();
    }
    if (result->is_error()) {
      return result->error_value();
    }

    return ZX_OK;
  }

  zx_status_t FreeAllCanvases() {
    while (!canvas_indices_.empty()) {
      uint8_t index = canvas_indices_.back();
      canvas_indices_.pop_back();
      fidl::WireResult result = canvas_client_->Free(index);
      if (!result.ok()) {
        return result.error().status();
      }
      if (result->is_error()) {
        return result->error_value();
      }
    }
    return ZX_OK;
  }

  void SetRegisterExpectations() {
    GetMockReg<CanvasLutDataLow>(mock_regs_).ExpectWrite(CanvasLutDataLowValue());
    GetMockReg<CanvasLutDataHigh>(mock_regs_).ExpectWrite(CanvasLutDataHighValue());
    GetMockReg<CanvasLutAddr>(mock_regs_).ExpectWrite(CanvasLutAddrValue(NextCanvasIndex()));
  }

  void SetRegisterExpectations(uint8_t index) {
    GetMockReg<CanvasLutDataLow>(mock_regs_).ExpectWrite(CanvasLutDataLowValue());
    GetMockReg<CanvasLutDataHigh>(mock_regs_).ExpectWrite(CanvasLutDataHighValue());
    GetMockReg<CanvasLutAddr>(mock_regs_).ExpectWrite(CanvasLutAddrValue(index));
  }

  void VerifyAll() {
    GetMockReg<CanvasLutDataLow>(mock_regs_).VerifyAndClear();
    GetMockReg<CanvasLutDataHigh>(mock_regs_).VerifyAndClear();
    GetMockReg<CanvasLutAddr>(mock_regs_).VerifyAndClear();
  }

 private:
  uint8_t NextCanvasIndex() { return static_cast<uint8_t>(canvas_indices_.size()); }

  uint32_t CanvasLutDataLowValue() {
    auto data_low = CanvasLutDataLow::Get().FromValue(0);
    data_low.SetDmcCavWidth(test_canvas_info.stride_bytes >> 3);
    data_low.set_dmc_cav_addr(FAKE_BTI_PHYS_ADDR >> 3);
    return data_low.reg_value();
  }

  uint32_t CanvasLutDataHighValue() {
    auto data_high = CanvasLutDataHigh::Get().FromValue(0);
    data_high.SetDmcCavWidth(test_canvas_info.stride_bytes >> 3);
    data_high.set_dmc_cav_height(test_canvas_info.height);
    data_high.set_dmc_cav_blkmode(static_cast<uint32_t>(test_canvas_info.blkmode));
    data_high.set_dmc_cav_xwrap(
        test_canvas_info.flags & fuchsia_hardware_amlogiccanvas::CanvasFlags::kWrapHorizontal ? 1
                                                                                              : 0);
    data_high.set_dmc_cav_ywrap(
        test_canvas_info.flags & fuchsia_hardware_amlogiccanvas::CanvasFlags::kWrapVertical ? 1
                                                                                            : 0);
    data_high.set_dmc_cav_endianness(static_cast<uint32_t>(test_canvas_info.endianness));
    return data_high.reg_value();
  }

  uint32_t CanvasLutAddrValue(uint8_t index) {
    auto lut_addr = CanvasLutAddr::Get().FromValue(0);
    lut_addr.set_dmc_cav_addr_index(index);
    lut_addr.set_dmc_cav_addr_wr(1);
    return lut_addr.reg_value();
  }

  fdf_testing::DriverRuntimeEnv env_;
  fdf::TestSynchronizedDispatcher dispatcher_{fdf::kDispatcherNoDefaultAllowSync};
  std::vector<uint8_t> canvas_indices_;
  ddk_mock::MockMmioRegRegion mock_regs_;
  async_patterns::TestDispatcherBound<AmlCanvasWrap> canvas_{dispatcher_.dispatcher(),
                                                             std::in_place};
  fidl::WireSyncClient<fuchsia_hardware_amlogiccanvas::Device> canvas_client_;
};

TEST_F(AmlCanvasTest, DdkLifecyle) { TestLifecycle(); }

TEST_F(AmlCanvasTest, CanvasConfigFreeSingle) {
  SetRegisterExpectations();
  EXPECT_OK(CreateNewCanvas());
  ASSERT_NO_FATAL_FAILURE(VerifyAll());

  EXPECT_OK(FreeAllCanvases());
}

TEST_F(AmlCanvasTest, CanvasConfigFreeMultipleSequential) {
  // Create 5 canvases in sequence and verify that their indices are 0 through 4.
  for (int i = 0; i < 5; i++) {
    SetRegisterExpectations();
    EXPECT_OK(CreateNewCanvas());
    ASSERT_NO_FATAL_FAILURE(VerifyAll());
  }

  // Free all 5 canvases created above.
  EXPECT_OK(FreeAllCanvases());
}

TEST_F(AmlCanvasTest, CanvasConfigFreeMultipleInterleaved) {
  // Create 5 canvases in sequence.
  for (int i = 0; i < 5; i++) {
    SetRegisterExpectations();
    EXPECT_OK(CreateNewCanvas());
    ASSERT_NO_FATAL_FAILURE(VerifyAll());
  }

  // Free canvas index 1, so the next one created has index 1.
  EXPECT_OK(FreeCanvas(1));

  SetRegisterExpectations(1);
  EXPECT_OK(CreateNewCanvas());
  ASSERT_NO_FATAL_FAILURE(VerifyAll());

  // Free canvas index 3, so the next one created has index 3.
  EXPECT_OK(FreeCanvas(3));

  SetRegisterExpectations(3);
  EXPECT_OK(CreateNewCanvas());
  ASSERT_NO_FATAL_FAILURE(VerifyAll());

  EXPECT_OK(FreeAllCanvases());
}

TEST_F(AmlCanvasTest, CanvasFreeInvalidIndex) {
  // Free a canvas without having created any.
  EXPECT_EQ(FreeCanvas(0), ZX_ERR_INVALID_ARGS);
}

TEST_F(AmlCanvasTest, CanvasConfigMaxLimit) {
  // Create canvases until the look-up table is full.
  for (size_t i = 0; i < kNumCanvasEntries; i++) {
    SetRegisterExpectations();
    EXPECT_OK(CreateNewCanvas());
    ASSERT_NO_FATAL_FAILURE(VerifyAll());
  }

  // Try to create another canvas, and verify that it fails.
  EXPECT_EQ(CreateNewCanvas(), ZX_ERR_NOT_FOUND);

  EXPECT_OK(FreeAllCanvases());
}

TEST_F(AmlCanvasTest, CanvasConfigUnaligned) {
  // Try to create a canvas with unaligned fuchsia_hardware_amlogiccanvas::wire::CanvasInfo width,
  // and verify that it fails.
  EXPECT_EQ(CreateNewCanvasInvalid(), ZX_ERR_INVALID_ARGS);
}

}  //  namespace aml_canvas
