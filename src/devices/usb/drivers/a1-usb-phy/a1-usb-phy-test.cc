// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/a1-usb-phy/a1-usb-phy.h"

#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
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
#include "src/devices/registers/testing/mock-registers/mock-registers.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/devices/usb/drivers/a1-usb-phy/a1_usb_phy_bind.h"
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
  FakeMmio() {
    for (size_t c = 0; c < kRegisterCount; c++) {
      regs_[c].SetReadCallback([this, c]() { return reg_values_[c]; });
      regs_[c].SetWriteCallback([this, c](uint64_t value) {
        reg_values_[c] = value;
        if (callback_) {
          (*callback_)(c, value);
        }
      });
    }
    region_.emplace(regs_, sizeof(uint32_t), kRegisterCount);
  }

  fdf::MmioBuffer mmio() { return region_->GetMmioBuffer(); }

 private:
  ddk_fake::FakeMmioReg regs_[kRegisterCount];
  std::optional<ddk_fake::FakeMmioRegRegion> region_;

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
    root_->SetMetadata(DEVICE_METADATA_PRIVATE, &kMagicNumbers, sizeof(kMagicNumbers));

    fake_pdev::FakePDevFidl::Config config;
    config.mmios[0] = mmio_[0].mmio();
    config.mmios[1] = mmio_[1].mmio();
    config.mmios[2] = mmio_[2].mmio();
    config.mmios[3] = mmio_[3].mmio();

    ASSERT_OK(fake_root_resource_create(smc_monitor_.reset_and_get_address()));
    config.smcs[0] = {};
    smc_monitor_.duplicate(ZX_RIGHT_SAME_RIGHTS, &config.smcs[0]);

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
                          std::move(outgoing_endpoints->client), "pdev");

    ASSERT_OK(A1UsbPhy::Create(nullptr, root_.get()));
  }

  void TearDown() override {}

 protected:
  std::shared_ptr<MockDevice> root_ = MockDevice::FakeRootParent();

 private:
  FakeMmio mmio_[kRegisterBanks];
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  zx::resource smc_monitor_;
};

}  // namespace a1_usb_phy
