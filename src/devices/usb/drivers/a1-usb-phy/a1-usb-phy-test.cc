// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/a1-usb-phy/a1-usb-phy.h"

#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/fake-resource/resource.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>

#include <list>
#include <memory>
#include <queue>
#include <thread>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>
#include <zxtest/zxtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/devices/usb/drivers/a1-usb-phy/usb-phy-regs.h"

namespace a1_usb_phy {

enum class RegisterIndex : size_t {
  Control = 0,
  Phy = 1,
  Reset = 2,
  Clk = 3,
};

constexpr auto kRegisterBanks = 4;
constexpr auto kRegisterCount = 2048;

class FakeMmio {
 public:
  FakeMmio() : region_(sizeof(uint32_t), kRegisterCount) {
    for (size_t c = 0; c < kRegisterCount; c++) {
      region_[c * sizeof(uint32_t)].SetReadCallback([this, c]() { return reg_values_[c]; });
      region_[c * sizeof(uint32_t)].SetWriteCallback([this, c](uint64_t value) {
        reg_values_[c] = value;
        if (callback_) {
          (*callback_)(c, value);
        }
      });
    }
  }

  fdf::MmioBuffer mmio() { return region_.GetMmioBuffer(); }

 private:
  ddk_fake::FakeMmioRegRegion region_;
  std::optional<fit::function<void(size_t reg, uint64_t value)>> callback_;
  uint64_t reg_values_[kRegisterCount] = {};
};

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

// Fixture that supports tests of A1UsbPhy::Create.
class A1UsbPhyTest : public zxtest::Test {
 public:
  A1UsbPhyTest() {
    static constexpr uint32_t kMagicNumbers[8] = {};
    A1UsbPhy::TestMetadata test_meta{.smc_short_circuit = true};
    usb_mode_t mode = USB_MODE_HOST;
    root_->SetMetadata(DEVICE_METADATA_PRIVATE, &kMagicNumbers, sizeof(kMagicNumbers));
    root_->SetMetadata(DEVICE_METADATA_TEST, &test_meta, sizeof(test_meta));
    root_->SetMetadata(DEVICE_METADATA_USB_MODE, &mode, sizeof(mode));

    fake_pdev::FakePDevFidl::Config config;
    config.mmios[0] = mmio_[0].mmio();  // MMIO_USB_CONTROL
    config.mmios[1] = mmio_[1].mmio();  // MMIO_USB_PHY
    config.mmios[2] = mmio_[2].mmio();  // MMIO_USB_RESET
    config.mmios[3] = mmio_[3].mmio();  // MMIO_USB_CLOCK

    auto& ctl_mmio = std::get<fdf::MmioBuffer>(config.mmios[0]);
    U2P_R1_V2::Get(0).FromValue(0).set_phy_rdy(1).WriteTo(&ctl_mmio);

    ASSERT_OK(fake_root_resource_create(root_resource_.reset_and_get_address()));

    zx::result outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(outgoing_endpoints);
    ASSERT_OK(incoming_loop_.StartThread("incoming-ns-thread"));
    incoming_.SyncCall([config = std::move(config), server = std::move(outgoing_endpoints->server)](
                           IncomingNamespace* infra) mutable {
      infra->pdev_server.SetConfig(std::move(config));
      ASSERT_OK(infra->outgoing.AddService<fuchsia_hardware_platform_device::Service>(
          infra->pdev_server.GetInstanceHandler()));
      ASSERT_OK(infra->outgoing.Serve(std::move(server)));
    });
    ASSERT_NO_FATAL_FAILURE();
    root_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                          std::move(outgoing_endpoints->client));

    ASSERT_OK(a1_usb_phy::A1UsbPhy::Create(nullptr, root_.get()));
    ASSERT_EQ(root_->child_count(), 1);
  }

  void TearDown() override {}

 protected:
  std::shared_ptr<MockDevice> root_ = MockDevice::FakeRootParent();

 private:
  FakeMmio mmio_[kRegisterBanks];
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  zx::resource root_resource_;
};

TEST_F(A1UsbPhyTest, XhciTest) {
  // The a1-usb-phy device should be added.
  ASSERT_EQ(root_->child_count(), 1);
  auto* mock_phy = root_->GetLatestChild();
  ASSERT_NOT_NULL(mock_phy);
  auto* phy = mock_phy->GetDeviceContext<A1UsbPhy>();

  // Call DdkInit
  mock_phy->InitOp();
  mock_phy->WaitUntilInitReplyCalled();
  EXPECT_EQ(mock_phy->child_count(), 1);
  EXPECT_TRUE(mock_phy->InitReplyCalled());

  auto* xhci = mock_phy->GetLatestChild();
  ASSERT_NOT_NULL(xhci);
  auto* xhci_ctx = xhci->GetDeviceContext<void>();
  ASSERT_EQ(xhci_ctx, phy);
  ASSERT_EQ(phy->mode(), A1UsbPhy::UsbMode::HOST);
}

}  // namespace a1_usb_phy
