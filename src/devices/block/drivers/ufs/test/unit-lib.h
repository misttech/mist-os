// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/test_base.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/power/cpp/testing/fake_element_control.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/driver_lifecycle.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/inspect/testing/cpp/zxtest/inspect.h>

#include <zxtest/zxtest.h>

#include "mock-device/ufs-mock-device.h"
#include "src/devices/block/drivers/ufs/ufs.h"

namespace ufs {

using fdf_power::testing::FakeElementControl;

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

class FakeSystemActivityGovernor
    : public fidl::testing::TestBase<fuchsia_power_system::ActivityGovernor> {
 public:
  FakeSystemActivityGovernor(zx::event exec_state_opportunistic, zx::event wake_handling_assertive)
      : exec_state_opportunistic_(std::move(exec_state_opportunistic)),
        wake_handling_assertive_(std::move(wake_handling_assertive)) {}

  fidl::ProtocolHandler<fuchsia_power_system::ActivityGovernor> CreateHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

  void GetPowerElements(GetPowerElementsCompleter::Sync& completer) override {
    fuchsia_power_system::PowerElements elements;
    zx::event execution_element;
    exec_state_opportunistic_.duplicate(ZX_RIGHT_SAME_RIGHTS, &execution_element);

    fuchsia_power_system::ExecutionState exec_state = {
        {.opportunistic_dependency_token = std::move(execution_element)}};

    elements = {{.execution_state = std::move(exec_state)}};

    completer.Reply({{std::move(elements)}});
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    ADD_FAILURE("%s is not implemented", name.c_str());
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_system::ActivityGovernor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor> bindings_;

  zx::event exec_state_opportunistic_;
  zx::event wake_handling_assertive_;
};

class FakeLessor : public fidl::Server<fuchsia_power_broker::Lessor> {
 public:
  void AddSideEffect(fit::function<void()> side_effect) { side_effect_ = std::move(side_effect); }

  void Lease(fuchsia_power_broker::LessorLeaseRequest& req,
             LeaseCompleter::Sync& completer) override {
    if (side_effect_) {
      side_effect_();
    }

    auto [lease_control_client_end, lease_control_server_end] =
        fidl::Endpoints<fuchsia_power_broker::LeaseControl>::Create();
    completer.Reply(fit::success(std::move(lease_control_client_end)));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Lessor> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

 private:
  fit::function<void()> side_effect_;
};

class FakeCurrentLevel : public fidl::Server<fuchsia_power_broker::CurrentLevel> {
 public:
  void Update(fuchsia_power_broker::CurrentLevelUpdateRequest& req,
              UpdateCompleter::Sync& completer) override {
    completer.Reply(fit::success());
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::CurrentLevel> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}
};

class FakeRequiredLevel : public fidl::Server<fuchsia_power_broker::RequiredLevel> {
 public:
  void Watch(WatchCompleter::Sync& completer) override {
    completer.Reply(fit::success(required_level_));
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::RequiredLevel> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  fuchsia_power_broker::PowerLevel required_level_ = Ufs::kPowerLevelOff;
};

class PowerElement {
 public:
  explicit PowerElement(
      fidl::ServerBindingRef<fuchsia_power_broker::ElementControl> element_control,
      fidl::ServerBindingRef<fuchsia_power_broker::Lessor> lessor,
      fidl::ServerBindingRef<fuchsia_power_broker::CurrentLevel> current_level,
      fidl::ServerBindingRef<fuchsia_power_broker::RequiredLevel> required_level)
      : element_control_(std::move(element_control)),
        lessor_(std::move(lessor)),
        current_level_(std::move(current_level)),
        required_level_(std::move(required_level)) {}

  fidl::ServerBindingRef<fuchsia_power_broker::ElementControl> element_control_;
  fidl::ServerBindingRef<fuchsia_power_broker::Lessor> lessor_;
  fidl::ServerBindingRef<fuchsia_power_broker::CurrentLevel> current_level_;
  fidl::ServerBindingRef<fuchsia_power_broker::RequiredLevel> required_level_;
};

class FakePowerBroker : public fidl::Server<fuchsia_power_broker::Topology> {
 public:
  fidl::ProtocolHandler<fuchsia_power_broker::Topology> CreateHandler() {
    return bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                   fidl::kIgnoreBindingClosure);
  }

  void AddElement(fuchsia_power_broker::ElementSchema& req,
                  AddElementCompleter::Sync& completer) override {
    // Get channels from request.
    ASSERT_TRUE(req.level_control_channels().has_value());
    fidl::ServerEnd<fuchsia_power_broker::CurrentLevel>& current_level_server_end =
        req.level_control_channels().value().current();
    fidl::ServerEnd<fuchsia_power_broker::RequiredLevel>& required_level_server_end =
        req.level_control_channels().value().required();
    fidl::ServerEnd<fuchsia_power_broker::Lessor>& lessor_server_end = req.lessor_channel().value();

    // Instantiate (fake) element control implementation.
    ASSERT_TRUE(req.element_control().has_value());
    auto element_control_impl = std::make_unique<FakeElementControl>();
    fidl::ServerBindingRef<fuchsia_power_broker::ElementControl> element_control_binding =
        fidl::BindServer<fuchsia_power_broker::ElementControl>(
            fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(*req.element_control()),
            std::move(element_control_impl));

    // Instantiate (fake) lessor implementation.
    auto lessor_impl = std::make_unique<FakeLessor>();
    if (req.element_name() == Ufs::kHardwarePowerElementName) {
      hardware_power_lessor_ = lessor_impl.get();
    } else if (req.element_name() == Ufs::kSystemWakeOnRequestPowerElementName) {
      wake_on_request_lessor_ = lessor_impl.get();
    } else {
      ZX_ASSERT_MSG(0, "Unexpected power element.");
    }
    fidl::ServerBindingRef<fuchsia_power_broker::Lessor> lessor_binding =
        fidl::BindServer<fuchsia_power_broker::Lessor>(
            fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(lessor_server_end),
            std::move(lessor_impl),
            [](FakeLessor* impl, fidl::UnbindInfo info,
               fidl::ServerEnd<fuchsia_power_broker::Lessor> server_end) mutable {});

    // Instantiate (fake) current and required level implementations.
    auto current_level_impl = std::make_unique<FakeCurrentLevel>();
    auto required_level_impl = std::make_unique<FakeRequiredLevel>();
    if (req.element_name() == Ufs::kHardwarePowerElementName) {
      hardware_power_current_level_ = current_level_impl.get();
      hardware_power_required_level_ = required_level_impl.get();
    } else if (req.element_name() == Ufs::kSystemWakeOnRequestPowerElementName) {
      wake_on_request_current_level_ = current_level_impl.get();
      wake_on_request_required_level_ = required_level_impl.get();
    } else {
      ZX_ASSERT_MSG(0, "Unexpected power element.");
    }
    fidl::ServerBindingRef<fuchsia_power_broker::CurrentLevel> current_level_binding =
        fidl::BindServer<fuchsia_power_broker::CurrentLevel>(
            fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(current_level_server_end),
            std::move(current_level_impl),
            [](FakeCurrentLevel* impl, fidl::UnbindInfo info,
               fidl::ServerEnd<fuchsia_power_broker::CurrentLevel> server_end) mutable {});
    fidl::ServerBindingRef<fuchsia_power_broker::RequiredLevel> required_level_binding =
        fidl::BindServer<fuchsia_power_broker::RequiredLevel>(
            fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(required_level_server_end),
            std::move(required_level_impl),
            [](FakeRequiredLevel* impl, fidl::UnbindInfo info,
               fidl::ServerEnd<fuchsia_power_broker::RequiredLevel> server_end) mutable {});

    if (wake_on_request_lessor_ && hardware_power_required_level_) {
      wake_on_request_lessor_->AddSideEffect(
          [&]() { hardware_power_required_level_->required_level_ = Ufs::kPowerLevelOn; });
    }

    servers_.emplace_back(std::move(element_control_binding), std::move(lessor_binding),
                          std::move(current_level_binding), std::move(required_level_binding));

    completer.Reply(fit::success());
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_power_broker::Topology> md,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  FakeLessor* hardware_power_lessor_ = nullptr;
  FakeCurrentLevel* hardware_power_current_level_ = nullptr;
  FakeRequiredLevel* hardware_power_required_level_ = nullptr;
  FakeLessor* wake_on_request_lessor_ = nullptr;
  FakeCurrentLevel* wake_on_request_current_level_ = nullptr;
  FakeRequiredLevel* wake_on_request_required_level_ = nullptr;

 private:
  fidl::ServerBindingGroup<fuchsia_power_broker::Topology> bindings_;

  std::vector<PowerElement> servers_;
};

struct IncomingNamespace {
  IncomingNamespace() {
    zx::event::create(0, &exec_opportunistic);
    zx::event::create(0, &wake_assertive);
    zx::event exec_opportunistic_dupe, wake_assertive_dupe;
    ASSERT_OK(exec_opportunistic.duplicate(ZX_RIGHT_SAME_RIGHTS, &exec_opportunistic_dupe));
    ASSERT_OK(wake_assertive.duplicate(ZX_RIGHT_SAME_RIGHTS, &wake_assertive_dupe));
    system_activity_governor.emplace(std::move(exec_opportunistic_dupe),
                                     std::move(wake_assertive_dupe));
  }

  fdf_testing::TestNode node{"root"};
  fdf_testing::internal::TestEnvironment env{fdf::Dispatcher::GetCurrent()->get()};
  FakePci pci_server;
  zx::event exec_opportunistic, wake_assertive;
  std::optional<FakeSystemActivityGovernor> system_activity_governor;
  FakePowerBroker power_broker;
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

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
// TODO(https://fxbug.dev/42075643): This should use fdf_testing::DriverTestFixture.
class UfsTest : public inspect::InspectTestHelper, public zxtest::Test {
 public:
  UfsTest()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        incoming_(env_dispatcher_->async_dispatcher(), std::in_place) {}

  void InitMockDevice();
  void StartDriver(bool supply_power_framework = false);

  void SetUp() override;

  void TearDown() override;

  zx::result<fdf::MmioBuffer> GetMmioBuffer(zx::vmo vmo) {
    return zx::ok(mock_device_.GetMmioBuffer(std::move(vmo)));
  }

  zx_status_t DisableController();
  zx_status_t EnableController();

  // Helper functions for accessing private functions.
  zx::result<> TransferFillDescriptorAndSendRequest(uint8_t slot, DataDirection ddir,
                                                    uint16_t resp_offset, uint16_t resp_len,
                                                    uint16_t prdt_offset,
                                                    uint16_t prdt_entry_count);
  zx::result<> TaskManagementFillDescriptorAndSendRequest(uint8_t slot,
                                                          TaskManagementRequestUpiu& request);

  // Map the data vmo to the address space and assign physical addresses. Currently, it only
  // supports 8KB vmo. So, we get two physical addresses. The return value is the physical address
  // of the pinned memory.
  zx::result<> MapVmo(zx::unowned_vmo& vmo, fzl::VmoMapper& mapper, uint64_t offset_vmo,
                      uint64_t length);

  uint8_t GetSlotStateCount(SlotState slot_state);

  zx::result<uint32_t> ReadAttribute(Attributes attribute, uint8_t index = 0);
  zx::result<> WriteAttribute(Attributes attribute, uint32_t value, uint8_t index = 0);

  zx::result<> DisableBackgroundOp() { return dut_->GetDeviceManager().DisableBackgroundOp(); }

  // This function is a wrapper to avoid the thread annotation of ReserveAdminSlot().
  zx::result<uint8_t> ReserveAdminSlot() {
    std::lock_guard<std::mutex> lock(dut_->GetTransferRequestProcessor().admin_slot_lock_);
    return dut_->GetTransferRequestProcessor().ReserveAdminSlot();
  }

  ufs_mock_device::UfsMockDevice mock_device_;

  template <class T>
  zx::result<uint8_t> ReserveSlot() {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  template <>
  zx::result<uint8_t> ReserveSlot<TransferRequestProcessor>() {
    return dut_->GetTransferRequestProcessor().ReserveSlot();
  }
  template <>
  zx::result<uint8_t> ReserveSlot<TaskManagementRequestProcessor>() {
    return dut_->GetTaskManagementRequestProcessor().ReserveSlot();
  }

  template <class T>
  zx::result<> RingRequestDoorbell(uint8_t slot_num) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  template <>
  zx::result<> RingRequestDoorbell<TransferRequestProcessor>(uint8_t slot_num) {
    return dut_->GetTransferRequestProcessor().RingRequestDoorbell(slot_num);
  }
  template <>
  zx::result<> RingRequestDoorbell<TaskManagementRequestProcessor>(uint8_t slot_num) {
    return dut_->GetTaskManagementRequestProcessor().RingRequestDoorbell(slot_num);
  }

 protected:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_;
  fdf_testing::internal::DriverUnderTest<TestUfs> dut_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_UNIT_LIB_H_
