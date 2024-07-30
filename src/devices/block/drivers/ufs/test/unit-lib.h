// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/inspect/testing/cpp/zxtest/inspect.h>

#include <zxtest/zxtest.h>

#include "mock-device/ufs-mock-device.h"
#include "src/devices/block/drivers/ufs/ufs.h"

namespace ufs {

// Implement all the WireServer handlers of fuchsia_hardware_pci::Device as protocol as required by
// FIDL.
class FakePci : public fidl::WireServer<fuchsia_hardware_pci::Device> {
 public:
  fuchsia_hardware_pci::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_pci::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(
            this, fdf::Dispatcher::GetCurrent()->async_dispatcher(), fidl::kIgnoreBindingClosure),
    });
  }

  void GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) override {
    fuchsia_hardware_pci::wire::DeviceInfo info;
    completer.Reply(info);
  }
  void GetBar(GetBarRequestView request, GetBarCompleter::Sync& completer) override {
    fuchsia_hardware_pci::wire::Bar bar = {
        .bar_id = 0,
        .size = RegisterMap::kRegisterSize,
        .result = fuchsia_hardware_pci::wire::BarResult::WithVmo(mock_device_->GetVmo()),
    };
    completer.ReplySuccess(std::move(bar));
  }
  void SetBusMastering(SetBusMasteringRequestView request,
                       SetBusMasteringCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }
  void ResetDevice(ResetDeviceCompleter::Sync& completer) override { completer.ReplySuccess(); }
  void AckInterrupt(AckInterruptCompleter::Sync& completer) override { completer.ReplySuccess(); }
  void MapInterrupt(MapInterruptRequestView request,
                    MapInterruptCompleter::Sync& completer) override {
    completer.ReplySuccess(mock_device_->GetIrq());
  }
  void GetInterruptModes(GetInterruptModesCompleter::Sync& completer) override {
    fuchsia_hardware_pci::wire::InterruptModes modes;
    modes.has_legacy = true;
    modes.msix_count = 0;
    modes.msi_count = 0;
    completer.Reply(modes);
  }
  void SetInterruptMode(SetInterruptModeRequestView request,
                        SetInterruptModeCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }
  void ReadConfig8(ReadConfig8RequestView request, ReadConfig8Completer::Sync& completer) override {
    completer.ReplySuccess(0);
  }
  void ReadConfig16(ReadConfig16RequestView request,
                    ReadConfig16Completer::Sync& completer) override {
    completer.ReplySuccess(0);
  }
  void ReadConfig32(ReadConfig32RequestView request,
                    ReadConfig32Completer::Sync& completer) override {
    completer.ReplySuccess(0);
  }
  void WriteConfig8(WriteConfig8RequestView request,
                    WriteConfig8Completer::Sync& completer) override {
    completer.ReplySuccess();
  }
  void WriteConfig16(WriteConfig16RequestView request,
                     WriteConfig16Completer::Sync& completer) override {
    completer.ReplySuccess();
  }
  void WriteConfig32(WriteConfig32RequestView request,
                     WriteConfig32Completer::Sync& completer) override {
    completer.ReplySuccess();
  }
  void GetCapabilities(GetCapabilitiesRequestView request,
                       GetCapabilitiesCompleter::Sync& completer) override {
    std::vector<uint8_t> empty_vec;
    auto empty_vec_view = fidl::VectorView<uint8_t>::FromExternal(empty_vec);
    completer.Reply(empty_vec_view);
  }
  void GetExtendedCapabilities(GetExtendedCapabilitiesRequestView request,
                               GetExtendedCapabilitiesCompleter::Sync& completer) override {
    std::vector<uint16_t> empty_vec;
    auto empty_vec_view = fidl::VectorView<uint16_t>::FromExternal(empty_vec);
    completer.Reply(empty_vec_view);
  }
  void GetBti(GetBtiRequestView request, GetBtiCompleter::Sync& completer) override {
    completer.ReplySuccess(mock_device_->GetFakeBti());
  }

  void SetMockDevice(ufs_mock_device::UfsMockDevice* mock_device) { mock_device_ = mock_device; }

  fidl::ServerBindingGroup<fuchsia_hardware_pci::Device> binding_group_;

  zx::interrupt irq_;
  ufs_mock_device::UfsMockDevice* mock_device_;
};

struct IncomingNamespace {
  fdf_testing::TestNode node{"root"};
  fdf_testing::TestEnvironment env{fdf::Dispatcher::GetCurrent()->get()};
  FakePci pci_server;
};

class TestUfs : public Ufs {
 public:
  TestUfs(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : Ufs(std::move(start_args), std::move(dispatcher)) {}
  ~TestUfs() override = default;

  inspect::ComponentInspector& GetInspector() { return inspector(); }

  static void SetMockDevice(ufs_mock_device::UfsMockDevice* mock_device) {
    TestUfs::mock_device_ = mock_device;
  }

 protected:
  zx::result<fdf::MmioBuffer> CreateMmioBuffer(zx_off_t offset, size_t size, zx::vmo vmo) override;

 private:
  // TODO(https://fxbug.dev/42075643): We can avoid the static pointer by moving the
  // RegisterMmioProcessor to the TestUfs class.
  static ufs_mock_device::UfsMockDevice* mock_device_;
};

// TODO(https://fxbug.dev/42075643): This should use fdf_testing::DriverTestFixture.
class UfsTest : public inspect::InspectTestHelper, public zxtest::Test {
 public:
  UfsTest()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        incoming_(env_dispatcher_->async_dispatcher(), std::in_place) {}

  void InitMockDevice();
  void StartDriver();

  void SetUp() override;

  void TearDown() override;

  zx::result<fdf::MmioBuffer> GetMmioBuffer(zx::vmo vmo) {
    return zx::ok(mock_device_.GetMmioBuffer(std::move(vmo)));
  }

  zx_status_t DisableController();
  zx_status_t EnableController();

  // Helper functions for accessing private functions.
  zx::result<> FillDescriptorAndSendRequest(uint8_t slot, DataDirection ddir, uint16_t resp_offset,
                                            uint16_t resp_len, uint16_t prdt_offset,
                                            uint16_t prdt_entry_count);

  // Map the data vmo to the address space and assign physical addresses. Currently, it only
  // supports 8KB vmo. So, we get two physical addresses. The return value is the physical address
  // of the pinned memory.
  zx::result<> MapVmo(zx::unowned_vmo& vmo, fzl::VmoMapper& mapper, uint64_t offset_vmo,
                      uint64_t length);

  uint8_t GetSlotStateCount(SlotState slot_state);

  zx::result<uint32_t> ReadAttribute(Attributes attribute, uint8_t index = 0);
  zx::result<> WriteAttribute(Attributes attribute, uint32_t value, uint8_t index = 0);

  ufs_mock_device::UfsMockDevice mock_device_;

 protected:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_;
  fdf_testing::DriverUnderTest<TestUfs> dut_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_
