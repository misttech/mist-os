// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/hardware/pciroot/cpp/banjo.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/vmo/types.h>
#include <lib/inspect/testing/cpp/inspect.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <unistd.h>
#include <zircon/limits.h>
#include <zircon/syscalls/object.h>

#include <memory>
#include <utility>

#include <fbl/ref_ptr.h>
#include <gtest/gtest.h>

#include "src/devices/bus/drivers/pci/capabilities.h"
#include "src/devices/bus/drivers/pci/capabilities/power_management.h"
#include "src/devices/bus/drivers/pci/config.h"
#include "src/devices/bus/drivers/pci/device.h"
#include "src/devices/bus/drivers/pci/test/fakes/fake_bus.h"
#include "src/devices/bus/drivers/pci/test/fakes/fake_config.h"
#include "src/devices/bus/drivers/pci/test/fakes/fake_pciroot.h"
#include "src/devices/bus/drivers/pci/test/fakes/fake_upstream_node.h"
#include "src/devices/bus/drivers/pci/test/fakes/test_device.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/lib/testing/predicates/status.h"
#include "test_helpers.h"

namespace pci {

// Creates a test device with a given device config using test defaults)

template <bool IsExtended>
class PciDeviceTests : protected ::pci_testing::InspectHelper, public ::testing::Test {
 public:
  static constexpr char kTestNodeName[] = "Test";
  static const char* name() { return kTestNodeName; }
  static pci_bdf_t bdf() { return {1, 2, 3}; }

 protected:
  FakeBus& bus() { return bus_; }
  FakeUpstreamNode& upstream() { return upstream_; }
  zx::vmo& inspect_vmo() { return inspect_vmo_; }
  inspect::Inspector& inspector() { return inspector_; }

  inspect::Node GetInspectNode() { return inspector_.GetRoot().CreateChild(name()); }

  Device& CreateTestDevice(zx_device_t* parent, const uint8_t* cfg_buf, size_t cfg_size) {
    // Copy the config dump into a device entry in the ecam.
    auto& ecam = bus_.pciroot().ecam();
    ecam.get_config_view(bdf()).WriteBuffer(0, cfg_buf, cfg_size);
    // Create the config object for the device.

    auto mmio_cfg = ecam.CreateMmioConfig(bdf());
    auto view = mmio_cfg->get_view();
    EXPECT_TRUE(view.is_ok());
    auto fake_cfg = std::make_unique<FakeMmioConfig>(bdf(), std::move(view.value()));
    // Create and initialize the fake device.
    EXPECT_OK(Device::Create(parent, std::move(fake_cfg), &upstream(), &bus(), GetInspectNode(),
                             /*has_acpi=*/false));
    return bus().get_device(bdf());
  }
  void ConfigureDownstreamDevices() { return upstream_.ConfigureDownstreamDevices(); }

  // TODO(https://fxbug.dev/42075363): Migrate test to use dispatcher integration.
  PciDeviceTests()
      : bus_(/*bus_start=*/0, /*bus_end=*/2, /*is_extended=*/IsExtended),
        upstream_(UpstreamNode::Type::ROOT, 0),
        inspect_vmo_(inspector_.DuplicateVmo()) {}
  ~PciDeviceTests() override {
    upstream_.DisableDownstream();
    upstream_.UnplugDownstream();
  }

 private:
  FakeBus bus_;
  FakeUpstreamNode upstream_;
  inspect::Inspector inspector_;
  zx::vmo inspect_vmo_;
};

using PciDeviceTestsCam = PciDeviceTests<false>;
using PciDeviceTestsExtendedCam = PciDeviceTests<true>;

extern "C" {
// MockDevice does not cover adding composite devices within a driver, but
// BanjoDevice:Create only needs to think it succeeded.
__EXPORT zx_status_t device_add_composite_spec(zx_device_t* dev, const char* name,
                                               const composite_node_spec_t* spec) {
  return ZX_OK;
}
}

// All tests below

TEST_F(PciDeviceTestsExtendedCam, CreationTest) {
  auto& ecam = bus().pciroot().ecam();
  // This test creates a device, goes through its init sequence, links it into
  // the toplogy, and then has it linger. It will be cleaned up by TearDown()
  // releasing all objects of upstream(). If creation succeeds here and no
  // asserts happen following the test it means the fakes are built properly
  // enough and the basic interface is fulfilled.
  ecam.get_config_view(bdf()).WriteBuffer(0, kFakeQuadroDeviceConfig.data(),
                                          kFakeQuadroDeviceConfig.max_size());
  auto mmio_cfg = MmioConfig::Create(bdf(), ecam.mmio(), ecam.bus_start(),
                                     ecam.bus_start() + ecam.bus_cnt(), ecam.is_extended());
  ASSERT_OK(mmio_cfg.status_value());
  auto view = mmio_cfg->get_view();
  EXPECT_TRUE(view.is_ok());
  // We need a FakeConfig here because we need BAR probing to be handled properly for inspect to be
  // populated.
  auto fake_cfg = std::make_unique<FakeMmioConfig>(bdf(), std::move(view.value()));
  ASSERT_OK(Device::Create(MockDevice::FakeRootParent().get(), std::move(fake_cfg), &upstream(),
                           &bus(), GetInspectNode(),
                           /*has_acpi=*/false));

  reinterpret_cast<FakeAllocator*>(&upstream().mmio_regions())->FailNextAllocation(true);
  reinterpret_cast<FakeAllocator*>(&upstream().pf_mmio_regions())->FailNextAllocation(true);
  ConfigureDownstreamDevices();

  // Verify the created device's BDF.
  auto& dev = bus().get_device(bdf());
  ASSERT_EQ(bdf().bus_id, dev.bus_id());
  ASSERT_EQ(bdf().device_id, dev.dev_id());
  ASSERT_EQ(bdf().function_id, dev.func_id());

  // Did the device BARs get allocated (and re-allocated) as expected?
  ASSERT_NO_FATAL_FAILURE(ReadInspect(inspect_vmo()));
  // We primed the MMIO allocators to fail the first round for BAR 0 so it
  // should have a 5th inspect entry for the failed allocation.
  EXPECT_EQ(5u, hierarchy()
                    .GetByPath({name(), pci::Device::Inspect::kInspectHeaderBars, "0"})
                    ->node()
                    .properties()
                    .size());
  EXPECT_EQ(4u, hierarchy()
                    .GetByPath({name(), pci::Device::Inspect::kInspectHeaderBars, "1"})
                    ->node()
                    .properties()
                    .size());
  EXPECT_EQ(4u, hierarchy()
                    .GetByPath({name(), pci::Device::Inspect::kInspectHeaderBars, "2"})
                    ->node()
                    .properties()
                    .size());
  EXPECT_EQ(4u, hierarchy()
                    .GetByPath({name(), pci::Device::Inspect::kInspectHeaderBars, "3"})
                    ->node()
                    .properties()
                    .size());
  // There should be no BAR 4, so no node at this path.
  EXPECT_EQ(nullptr,
            hierarchy().GetByPath({name(), pci::Device::Inspect::kInspectHeaderBars, "4"}));
  EXPECT_EQ(4u, hierarchy()
                    .GetByPath({name(), pci::Device::Inspect::kInspectHeaderBars, "5"})
                    ->node()
                    .properties()
                    .size());
}

// Test a normal capability chain
TEST_F(PciDeviceTestsCam, StdCapabilityTest) {
  // Copy the config dump into a device entry in the ecam.
  auto& ecam = bus().pciroot().ecam();
  ecam.get_config_view(bdf()).WriteBuffer(0, kFakeVirtioInputDeviceConfig.data(),
                                          kFakeVirtioInputDeviceConfig.max_size());
  auto cfg = ecam.CreateMmioConfig(bdf());
  ASSERT_OK(Device::Create(MockDevice::FakeRootParent().get(), std::move(cfg), &upstream(), &bus(),
                           GetInspectNode(),
                           /*has_acpi=*/false));
  auto& dev = bus().get_device(bdf());

  // Ensure our faked Keyboard exists.
  ASSERT_EQ(0x1af4u, dev.vendor_id());
  ASSERT_EQ(0x1052u, dev.device_id());

  // Since this is a dump of an emulated device we know it has a single MSI-X
  // capability followed by five Vendor capabilities.
  auto cap_iter = dev.capabilities().list.begin();
  EXPECT_EQ(static_cast<Capability::Id>(cap_iter->id()), Capability::Id::kMsiX);
  ASSERT_TRUE(cap_iter != dev.capabilities().list.end());
  EXPECT_EQ(static_cast<Capability::Id>((++cap_iter)->id()), Capability::Id::kVendor);
  ASSERT_TRUE(cap_iter != dev.capabilities().list.end());
  EXPECT_EQ(static_cast<Capability::Id>((++cap_iter)->id()), Capability::Id::kVendor);
  ASSERT_TRUE(cap_iter != dev.capabilities().list.end());
  EXPECT_EQ(static_cast<Capability::Id>((++cap_iter)->id()), Capability::Id::kVendor);
  ASSERT_TRUE(cap_iter != dev.capabilities().list.end());
  EXPECT_EQ(static_cast<Capability::Id>((++cap_iter)->id()), Capability::Id::kVendor);
  ASSERT_TRUE(cap_iter != dev.capabilities().list.end());
  EXPECT_EQ(static_cast<Capability::Id>((++cap_iter)->id()), Capability::Id::kVendor);
  EXPECT_TRUE(++cap_iter == dev.capabilities().list.end());
}

// Test an extended capability chain
TEST_F(PciDeviceTestsExtendedCam, ExtendedCapabilityTest) {
  auto& dev = CreateTestDevice(MockDevice::FakeRootParent().get(), kFakeQuadroDeviceConfig.data(),
                               kFakeQuadroDeviceConfig.max_size());
  ASSERT_EQ(false, HasNonfatalFailure());

  // Since this is a dump of an emulated device we that it should have:
  //
  //      Capabilities: [100] Virtual Channel
  //      Capabilities: [250] Latency Tolerance Reporting
  //      Capabilities: [258] L1 PM Substates
  //      Capabilities: [128] Power Budgeting
  //      Capabilities: [600] Vendor Specific Information
  auto cap_iter = dev.capabilities().ext_list.begin();
  ASSERT_TRUE(cap_iter.IsValid());
  EXPECT_EQ(static_cast<ExtCapability::Id>(cap_iter->id()),
            ExtCapability::Id::kVirtualChannelNoMFVC);
  ASSERT_TRUE(cap_iter != dev.capabilities().ext_list.end());
  EXPECT_EQ(static_cast<ExtCapability::Id>((++cap_iter)->id()),
            ExtCapability::Id::kLatencyToleranceReporting);
  ASSERT_TRUE(cap_iter != dev.capabilities().ext_list.end());
  EXPECT_EQ(static_cast<ExtCapability::Id>((++cap_iter)->id()), ExtCapability::Id::kL1PMSubstates);
  ASSERT_TRUE(cap_iter != dev.capabilities().ext_list.end());
  EXPECT_EQ(static_cast<ExtCapability::Id>((++cap_iter)->id()), ExtCapability::Id::kPowerBudgeting);
  ASSERT_TRUE(cap_iter != dev.capabilities().ext_list.end());
  EXPECT_EQ(static_cast<ExtCapability::Id>((++cap_iter)->id()), ExtCapability::Id::kVendor);
  EXPECT_TRUE(++cap_iter == dev.capabilities().ext_list.end());
}

// This test checks for proper handling of capability pointers that are
// invalid by pointing to inside the config header.
TEST_F(PciDeviceTestsCam, InvalidPtrCapabilityTest) {
  FakeBus bus(0, 2, true);
  FakeEcam& ecam = bus.pciroot().ecam();
  auto raw_cfg = bus.pciroot().ecam().get_config_view(bdf());
  auto* fake_dev = bus.pciroot().ecam().get_device(bdf());

  // Two valid locations, followed by a third capability pointing at BAR 1.
  const uint8_t kCap1 = 0x80;
  const uint8_t kCap2 = 0x90;
  const uint8_t kInvalidCap = 0x10;

  // Point to 0x80 as the first capability.
  fake_dev->set_vendor_id(0x8086)
      .set_device_id(0x1234)
      .set_capabilities_list(1)
      .set_capabilities_ptr(kCap1);
  raw_cfg.Write(static_cast<uint8_t>(Capability::Id::kPciPowerManagement), kCap1);
  raw_cfg.Write(kCap2, kCap1 + 1);
  raw_cfg.Write(static_cast<uint8_t>(Capability::Id::kMsiX), kCap2);
  raw_cfg.Write(kInvalidCap, kCap2 + 1);

  auto cfg = MmioConfig::Create(bdf(), ecam.mmio(), ecam.bus_start(), 2, ecam.is_extended());
  ASSERT_OK(cfg.status_value());
  EXPECT_EQ(ZX_ERR_OUT_OF_RANGE,
            Device::Create(MockDevice::FakeRootParent().get(), std::move(cfg.value()), &upstream(),
                           &bus, GetInspectNode(), /*has_acpi=*/false));

  // Ensure no device was added.
  EXPECT_TRUE(bus.devices().is_empty());
}

// This test checks for proper handling (ZX_ERR_BAD_STATE) upon
// funding a pointer cycle while parsing capabilities.
TEST_F(PciDeviceTestsCam, PtrCycleCapabilityTest) {
  // Boilerplate to.get_device a corresponding to the bdf().
  auto raw_cfg = bus().pciroot().ecam().get_config_view(bdf());
  auto* fake_dev = bus().pciroot().ecam().get_device(bdf());

  // Two valid locations, followed by a third capability pointing at BAR 1.
  const uint8_t kCap1 = 0x80;
  const uint8_t kCap2 = 0x90;
  const uint8_t kCap3 = 0xA0;

  // Create a Cycle of Cap1 -> Cap2 -> Cap3 -> Cap1
  fake_dev->set_vendor_id(0x8086)
      .set_device_id(0x1234)
      .set_capabilities_list(1)
      .set_capabilities_ptr(kCap1);
  auto cap_id = static_cast<uint8_t>(Capability::Id::kVendor);
  raw_cfg.Write(cap_id, kCap1);
  raw_cfg.Write(kCap2, kCap1 + 1);
  raw_cfg.Write(cap_id, kCap2);
  raw_cfg.Write(kCap3, kCap2 + 1);
  raw_cfg.Write(cap_id, kCap3);
  raw_cfg.Write(kCap1, kCap3 + 1);

  auto cfg = bus().pciroot().ecam().CreateMmioConfig(bdf());
  EXPECT_EQ(ZX_ERR_BAD_STATE,
            Device::Create(MockDevice::FakeRootParent().get(), std::move(cfg), &upstream(), &bus(),
                           GetInspectNode(), /*has_acpi=*/false));

  // Ensure no device was added.
  EXPECT_TRUE(bus().devices().is_empty());
}

// Test that we properly bail out if we see multiple of a capability
// type that only one should exist of in a system.
TEST_F(PciDeviceTestsCam, DuplicateFixedCapabilityTest) {
  // Boilerplate to.get_device a corresponding to the bdf().
  auto raw_cfg = bus().pciroot().ecam().get_config_view(bdf());
  auto* fake_dev = bus().pciroot().ecam().get_device(bdf());

  // Two valid locations, followed by a third capability pointing at BAR 1.
  const uint8_t kCap1 = 0x80;
  const uint8_t kCap2 = 0x90;
  const uint8_t kCap3 = 0xA0;

  // Create a device with three capabilities, two of which are kPciExpress
  fake_dev->set_vendor_id(0x8086)
      .set_device_id(0x1234)
      .set_capabilities_list(1)
      .set_capabilities_ptr(kCap1);
  auto pcie_id = static_cast<uint8_t>(Capability::Id::kPciExpress);
  auto null_id = static_cast<uint8_t>(Capability::Id::kNull);
  raw_cfg.Write8(pcie_id, kCap1);
  raw_cfg.Write8(kCap2, kCap1 + 1);
  raw_cfg.Write8(null_id, kCap2);
  raw_cfg.Write8(kCap3, kCap2 + 1);
  raw_cfg.Write8(pcie_id, kCap3);
  raw_cfg.Write8(0, kCap3 + 1);

  auto cfg = bus().pciroot().ecam().CreateMmioConfig(bdf());
  EXPECT_EQ(ZX_ERR_BAD_STATE,
            Device::Create(MockDevice::FakeRootParent().get(), std::move(cfg), &upstream(), &bus(),
                           GetInspectNode(), /*has_acpi=*/false));

  // Ensure no device was added.
  EXPECT_TRUE(bus().devices().is_empty());
}

// Ensure we parse MSI capabilities properly in the Quadro device.
// lspci output: Capabilities: [68] MSI: Enable+ Count=1/4 Maskable- 64bit+
TEST_F(PciDeviceTestsExtendedCam, MsiCapabilityTest) {
  auto& dev = CreateTestDevice(MockDevice::FakeRootParent().get(), kFakeQuadroDeviceConfig.data(),
                               kFakeQuadroDeviceConfig.max_size());
  ASSERT_EQ(false, HasNonfatalFailure());
  ASSERT_NE(nullptr, dev.capabilities().msi);

  auto& msi = *dev.capabilities().msi;
  EXPECT_EQ(0x68u, msi.base());
  EXPECT_EQ(static_cast<uint8_t>(Capability::Id::kMsi), msi.id());
  EXPECT_EQ(true, msi.is_64bit());
  EXPECT_EQ(4u, msi.vectors_avail());
  EXPECT_EQ(false, msi.supports_pvm());

  // MSI should be disabled by Device initialization.
  const MsiControlReg ctrl = {.value = dev.config()->Read(msi.ctrl())};
  EXPECT_EQ(0, ctrl.enable());
}

// Ensure we parse MSIX capabilities properly in the Virtio-input device.
TEST_F(PciDeviceTestsExtendedCam, MsixCapabilityTest) {
  auto& dev =
      CreateTestDevice(MockDevice::FakeRootParent().get(), kFakeVirtioInputDeviceConfig.data(),
                       kFakeVirtioInputDeviceConfig.max_size());
  ASSERT_EQ(false, HasNonfatalFailure());
  ASSERT_NE(nullptr, dev.capabilities().msix);

  auto& msix = *dev.capabilities().msix;
  EXPECT_EQ(0x98u, msix.base());
  EXPECT_EQ(static_cast<uint8_t>(Capability::Id::kMsiX), msix.id());
  EXPECT_EQ(1u, msix.table_bar());
  EXPECT_EQ(0u, msix.table_offset());
  EXPECT_EQ(2u, msix.table_size());
  EXPECT_EQ(1u, msix.pba_bar());
  EXPECT_EQ(0x800u, msix.pba_offset());

  // MSI-X should be disabled by Device initialization.
  const MsixControlReg ctrl = {.value = dev.config()->Read(msix.ctrl())};
  EXPECT_EQ(0u, ctrl.enable());
}

TEST_F(PciDeviceTestsExtendedCam, InspectIrqMode) {
  auto& dev = CreateTestDevice(MockDevice::FakeRootParent().get(), kFakeQuadroDeviceConfig.data(),
                               kFakeQuadroDeviceConfig.max_size());
  {
    const auto mode = fuchsia_hardware_pci::InterruptMode::kLegacy;
    ASSERT_OK(dev.SetIrqMode(mode, 1));
    ASSERT_NO_FATAL_FAILURE(ReadInspect(inspect_vmo()));
    auto* node = hierarchy().GetByPath({name(), pci::Device::Inspect::kInspectHeaderInterrupts});
    ASSERT_NO_FATAL_FAILURE(CheckProperty(
        node->node(), Device::Inspect::kInspectIrqMode,
        inspect::StringPropertyValue(Device::Inspect::kInspectIrqModes[fidl::ToUnderlying(mode)])));
    ASSERT_NO_FATAL_FAILURE(CheckProperty(node->node(), Device::Inspect::kInspectLegacyInterruptPin,
                                          inspect::StringPropertyValue("A")));
    ASSERT_NO_FATAL_FAILURE(CheckProperty(node->node(),
                                          Device::Inspect::kInspectLegacyInterruptLine,
                                          inspect::UintPropertyValue(16)));
  }
  {
    const auto mode = fuchsia_hardware_pci::InterruptMode::kLegacyNoack;
    ASSERT_OK(dev.SetIrqMode(mode, 1));
    ASSERT_NO_FATAL_FAILURE(ReadInspect(inspect_vmo()));
    auto* node = hierarchy().GetByPath({name(), pci::Device::Inspect::kInspectHeaderInterrupts});
    ASSERT_NO_FATAL_FAILURE(CheckProperty(
        node->node(), Device::Inspect::kInspectIrqMode,
        inspect::StringPropertyValue(Device::Inspect::kInspectIrqModes[fidl::ToUnderlying(mode)])));
  }
  {
    const auto mode = fuchsia_hardware_pci::InterruptMode::kMsi;
    ASSERT_OK(dev.SetIrqMode(mode, 1));
    ASSERT_NO_FATAL_FAILURE(ReadInspect(inspect_vmo()));
    auto* node = hierarchy().GetByPath({name(), pci::Device::Inspect::kInspectHeaderInterrupts});
    ASSERT_NO_FATAL_FAILURE(CheckProperty(
        node->node(), Device::Inspect::kInspectIrqMode,
        inspect::StringPropertyValue(Device::Inspect::kInspectIrqModes[fidl::ToUnderlying(mode)])));
  }

  {
    const auto mode = fuchsia_hardware_pci::InterruptMode::kMsiX;
    ASSERT_OK(dev.SetIrqMode(mode, 1));
    ASSERT_NO_FATAL_FAILURE(ReadInspect(inspect_vmo()));
    auto* node = hierarchy().GetByPath({name(), pci::Device::Inspect::kInspectHeaderInterrupts});
    ASSERT_NO_FATAL_FAILURE(CheckProperty(
        node->node(), Device::Inspect::kInspectIrqMode,
        inspect::StringPropertyValue(Device::Inspect::kInspectIrqModes[fidl::ToUnderlying(mode)])));
  }
}

TEST_F(PciDeviceTestsExtendedCam, InspectLegacy) {
  // Signal and Ack the legacy IRQ once each to ensure add is happening.
  pci::Device& dev =
      CreateTestDevice(MockDevice::FakeRootParent().get(), kFakeQuadroDeviceConfig.data(),
                       kFakeQuadroDeviceConfig.max_size());
  const auto mode = fuchsia_hardware_pci::InterruptMode::kLegacy;
  ASSERT_OK(dev.SetIrqMode(mode, 1));
  {
    const fbl::AutoLock _(dev.dev_lock());
    ASSERT_OK(dev.SignalLegacyIrq(0x10000));
    ASSERT_OK(dev.AckLegacyIrq());
  }

  // Verify properties in the general case.
  {
    ASSERT_NO_FATAL_FAILURE(ReadInspect(inspect_vmo()));
    auto& node = hierarchy().GetByPath({name(), Device::Inspect::kInspectHeaderInterrupts})->node();
    ASSERT_NO_FATAL_FAILURE(CheckProperty(node, Device::Inspect::kInspectLegacyInterruptPin,
                                          inspect::StringPropertyValue("A")));
    ASSERT_NO_FATAL_FAILURE(CheckProperty(node, Device::Inspect::kInspectLegacyInterruptLine,
                                          inspect::UintPropertyValue(dev.legacy_vector())));
    ASSERT_NO_FATAL_FAILURE(CheckProperty(node, Device::Inspect::kInspectLegacyAckCount,
                                          inspect::UintPropertyValue(1)));
    ASSERT_NO_FATAL_FAILURE(CheckProperty(node, Device::Inspect::kInspectLegacySignalCount,
                                          inspect::UintPropertyValue(1)));
  }

  {
    const auto mode = fuchsia_hardware_pci::InterruptMode::kDisabled;
    ASSERT_OK(dev.SetIrqMode(mode, 0));
    ASSERT_NO_FATAL_FAILURE(ReadInspect(inspect_vmo()));
    auto* node = hierarchy().GetByPath({name(), pci::Device::Inspect::kInspectHeaderInterrupts});
    ASSERT_NO_FATAL_FAILURE(CheckProperty(
        node->node(), Device::Inspect::kInspectIrqMode,
        inspect::StringPropertyValue(Device::Inspect::kInspectIrqModes[fidl::ToUnderlying(mode)])));
  }
}

TEST_F(PciDeviceTestsExtendedCam, InspectMsi) {
  const uint32_t irq_cnt = 4;
  pci::Device& dev =
      CreateTestDevice(MockDevice::FakeRootParent().get(), kFakeQuadroDeviceConfig.data(),
                       kFakeQuadroDeviceConfig.max_size());
  const auto mode = fuchsia_hardware_pci::InterruptMode::kMsiX;
  ASSERT_OK(dev.SetIrqMode(mode, irq_cnt));

  zx_info_msi_t info{};
  {
    const fbl::AutoLock _(dev.dev_lock());
    dev.msi_allocation().get_info(ZX_INFO_MSI, &info, sizeof(info), nullptr, nullptr);
  }

  ASSERT_NO_FATAL_FAILURE(ReadInspect(inspect_vmo()));
  auto& node =
      hierarchy().GetByPath({name(), pci::Device::Inspect::kInspectHeaderInterrupts})->node();
  ASSERT_NO_FATAL_FAILURE(CheckProperty(node, Device::Inspect::kInspectMsiBaseVector,
                                        inspect::UintPropertyValue(info.base_irq_id)));
  ASSERT_NO_FATAL_FAILURE(CheckProperty(node, Device::Inspect::kInspectMsiAllocated,
                                        inspect::UintPropertyValue(irq_cnt)));
}

// Verify that power state transitions wait the necessary amount of time, and that they end up in
// the correct state.
TEST_F(PciDeviceTestsExtendedCam, PowerStateTransitions) {
  pci::Device& dev =
      CreateTestDevice(MockDevice::FakeRootParent().get(), kFakeQuadroDeviceConfig.data(),
                       kFakeQuadroDeviceConfig.max_size());
  pci::Config& cfg = *dev.config();

  auto power = PowerManagementCapability(*dev.config(), kFakeQuadroPowerManagementCapabilityOffset);
  auto test_recovery_delay = [&cfg, &power](
                                 PowerManagementCapability::PowerState start_state,
                                 PowerManagementCapability::PowerState end_state) -> bool {
    // Manually update our starting state
    PmcsrReg pmcsr{.value = cfg.Read(power.pmcsr())};
    pmcsr.set_power_state(start_state);
    cfg.Write(power.pmcsr(), pmcsr.value);
    // Time the transition
    const zx::time start_time = zx::clock::get_monotonic();
    power.SetPowerState(cfg, end_state);
    const zx::time end_time = zx::clock::get_monotonic();
    const zx::duration min_delay =
        PowerManagementCapability::kStateRecoveryTime[start_state][end_state];
    return (end_time - start_time > min_delay);
  };

  ASSERT_TRUE(test_recovery_delay(PowerManagementCapability::PowerState::D0,
                                  PowerManagementCapability::PowerState::D1));
  ASSERT_EQ(dev.GetPowerState().value(), PowerManagementCapability::PowerState::D1);

  ASSERT_TRUE(test_recovery_delay(PowerManagementCapability::PowerState::D0,
                                  PowerManagementCapability::PowerState::D2));
  ASSERT_EQ(dev.GetPowerState().value(), PowerManagementCapability::PowerState::D2);

  ASSERT_TRUE(test_recovery_delay(PowerManagementCapability::PowerState::D0,
                                  PowerManagementCapability::PowerState::D3));
  ASSERT_EQ(dev.GetPowerState().value(), PowerManagementCapability::PowerState::D3);

  ASSERT_TRUE(test_recovery_delay(PowerManagementCapability::PowerState::D3,
                                  PowerManagementCapability::PowerState::D0));
  ASSERT_EQ(dev.GetPowerState().value(), PowerManagementCapability::PowerState::D0);

  // D0 to D0 should be essentially a no-op return.
  ASSERT_TRUE(test_recovery_delay(PowerManagementCapability::PowerState::D0,
                                  PowerManagementCapability::PowerState::D0));
  ASSERT_EQ(dev.GetPowerState().value(), PowerManagementCapability::PowerState::D0);

  // D1 to D2 should actually run D1 > D0 > D2 and hit both code paths.
  ASSERT_TRUE(test_recovery_delay(PowerManagementCapability::PowerState::D2,
                                  PowerManagementCapability::PowerState::D1));
  ASSERT_EQ(dev.GetPowerState().value(), PowerManagementCapability::PowerState::D1);
}

}  // namespace pci
