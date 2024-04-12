// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/dwc3/dwc3.h"

#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <zxtest/zxtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/devices/usb/drivers/dwc3/dwc3-regs.h"

namespace dwc3 {

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

class TestFixture : public zxtest::Test {
 public:
  TestFixture();
  void SetUp() override;

  fdf::MmioBuffer mmio() { return reg_region_.GetMmioBuffer(); }

 protected:
  static constexpr size_t kRegSize = sizeof(uint32_t);
  static constexpr size_t kMmioRegionSize = 64 << 10;
  static constexpr size_t kRegCount = kMmioRegionSize / kRegSize;

  // Section 1.3.22 of the DWC3 Programmer's guide
  //
  // DWC_USB31_CACHE_TOTAL_XFER_RESOURCES : 32
  // DWC_USB31_NUM_IN_EPS                 : 16
  // DWC_USB31_NUM_EPS                    : 32
  // DWC_USB31_VENDOR_CTL_INTERFACE       : 0
  // DWC_USB31_HSPHY_DWIDTH               : 2
  // DWC_USB31_HSPHY_INTERFACE            : 1
  // DWC_USB31_SSPHY_INTERFACE            : 2
  //
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

  std::shared_ptr<MockDevice> mock_parent_{MockDevice::FakeRootParent()};
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  ddk_fake::FakeMmioRegRegion reg_region_{kRegSize, kRegCount};
};

TestFixture::TestFixture() {
  auto& hwparams3 = reg_region_[GHWPARAMS3::Get().addr()];
  auto& ver_reg = reg_region_[USB31_VER_NUMBER::Get().addr()];
  auto& dctl_reg = reg_region_[DCTL::Get().addr()];

  hwparams3.SetReadCallback([this]() -> uint64_t { return Read_GHWPARAMS3(); });
  ver_reg.SetReadCallback([this]() -> uint64_t { return Read_USB31_VER_NUMBER(); });
  dctl_reg.SetReadCallback([this]() -> uint64_t { return Read_DCTL(); });
  dctl_reg.SetWriteCallback([this](uint64_t val) { return Write_DCTL(val); });

  fake_pdev::FakePDevFidl::Config config;
  config.mmios[0] = mmio();
  config.use_fake_bti = true;
  config.irqs[0] = {};
  ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]));

  auto outgoing_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
  ASSERT_OK(incoming_loop_.StartThread("incoming-ns-thread"));
  incoming_.SyncCall([config = std::move(config), server = std::move(outgoing_endpoints.server)](
                         IncomingNamespace* infra) mutable {
    infra->pdev_server.SetConfig(std::move(config));
    ASSERT_OK(infra->outgoing.AddService<fuchsia_hardware_platform_device::Service>(
        infra->pdev_server.GetInstanceHandler()));

    ASSERT_OK(infra->outgoing.Serve(std::move(server)));
  });
  ASSERT_NO_FATAL_FAILURE();
  mock_parent_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                               std::move(outgoing_endpoints.client));
}

void TestFixture::SetUp() { stuck_reset_test_ = false; }

TEST_F(TestFixture, DdkLifecycle) {
  ASSERT_OK(Dwc3::Create(nullptr, mock_parent_.get()));

  // make sure the child device is there
  ASSERT_EQ(1, mock_parent_->child_count());
  auto* child = mock_parent_->GetLatestChild();

  child->InitOp();
  EXPECT_TRUE(child->InitReplyCalled());
  EXPECT_OK(child->InitReplyCallStatus());

  child->UnbindOp();
  EXPECT_TRUE(child->UnbindReplyCalled());

  child->ReleaseOp();
}

TEST_F(TestFixture, DdkHwResetTimeout) {
  stuck_reset_test_ = true;
  ASSERT_OK(Dwc3::Create(nullptr, mock_parent_.get()));

  // make sure the child device is there
  ASSERT_EQ(1, mock_parent_->child_count());
  auto* child = mock_parent_->GetLatestChild();

  child->InitOp();
  EXPECT_TRUE(child->InitReplyCalled());
  EXPECT_STATUS(ZX_ERR_TIMED_OUT, child->InitReplyCallStatus());

  child->UnbindOp();
  EXPECT_TRUE(child->UnbindReplyCalled());

  child->ReleaseOp();
}

}  // namespace dwc3
