// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.pci/cpp/wire.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fake-bti/bti.h>
#include <zircon/system/ulib/async-default/include/lib/async/default.h>

#include <gtest/gtest.h>

#include "src/connectivity/wlan/drivers/third_party/intel/iwlwifi/platform/pcie-iwlwifi-driver.h"

namespace {

constexpr int kTestDeviceId = 0x095a;
constexpr int kTestSubsysDeviceId = 0x9e10;

// Implement all the WireServer handlers of fuchsia_hardware_pci::Device as protocol as required by
// FIDL.
class FakePciParent : public fidl::WireServer<fuchsia_hardware_pci::Device> {
 public:
  fuchsia_hardware_pci::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_pci::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(
            this, fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_get_current_dispatcher()),
            fidl::kIgnoreBindingClosure),
    });
  }

  void GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) override {
    fuchsia_hardware_pci::wire::DeviceInfo info;
    info.device_id = kTestDeviceId;
    completer.Reply(info);
  }
  void GetBar(GetBarRequestView request, GetBarCompleter::Sync& completer) override {
    fuchsia_hardware_pci::wire::Bar bar;
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
    zx::interrupt interrupt;
    completer.ReplySuccess(std::move(interrupt));
  }

  void GetInterruptModes(GetInterruptModesCompleter::Sync& completer) override {
    fuchsia_hardware_pci::wire::InterruptModes modes;
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
    // Always return the fake sub-system device id to pass the initialization.
    completer.ReplySuccess(kTestSubsysDeviceId);
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
    std::vector<uint8_t> dummy_vec;
    auto dummy_vec_view = fidl::VectorView<uint8_t>::FromExternal(dummy_vec);
    completer.Reply(dummy_vec_view);
  }

  void GetExtendedCapabilities(GetExtendedCapabilitiesRequestView request,
                               GetExtendedCapabilitiesCompleter::Sync& completer) override {
    std::vector<uint16_t> dummy_vec;
    auto dummy_vec_view = fidl::VectorView<uint16_t>::FromExternal(dummy_vec);
    completer.Reply(dummy_vec_view);
  }

  void GetBti(GetBtiRequestView request, GetBtiCompleter::Sync& completer) override {
    zx_handle_t fake_handle;
    fake_bti_create(&fake_handle);
    zx::bti bti(fake_handle);
    completer.ReplySuccess(std::move(bti));
  }

  fidl::ServerBindingGroup<fuchsia_hardware_pci::Device> binding_group_;
};

class TestNodeLocal : public fdf_testing::TestNode {
 public:
  TestNodeLocal(std::string name) : fdf_testing::TestNode::TestNode(name) {}
  size_t GetchildrenCount() { return children().size(); }
};

class TestEnvironmentLocal : public fdf_testing::TestEnvironment {
 public:
  zx::result<> Initialize(fidl::ServerEnd<fuchsia_io::Directory> incoming_directory_server_end) {
    return fdf_testing::TestEnvironment::Initialize(std::move(incoming_directory_server_end));
  }

  void AddService(fuchsia_hardware_pci::Service::InstanceHandler&& handler) {
    zx::result result =
        incoming_directory().AddService<fuchsia_hardware_pci::Service>(std::move(handler));
    EXPECT_TRUE(result.is_ok());
  }
};

class DriverLifeCycleTest : public ::testing::Test {
 public:
  void SetUp() override {
    // Create start args
    zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    EXPECT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment with incoming directory returned from the start args
    zx::result init_result =
        test_environment_.SyncCall(&fdf_testing::TestEnvironment::Initialize,
                                   std::move(start_args->incoming_directory_server));
    EXPECT_EQ(ZX_OK, init_result.status_value());

    // Get service handler from the fake_pci_parent_ object.
    auto handler = fake_pci_parent_.SyncCall(&FakePciParent::GetInstanceHandler);

    test_environment_.SyncCall(&TestEnvironmentLocal::AddService, std::move(handler));

    zx::result start_result =
        runtime_.RunToCompletion(driver_.Start(std::move(start_args->start_args)));
    EXPECT_EQ(ZX_OK, start_result.status_value());
  }

  void DriverPrepareStop() {
    zx::result prepare_stop_result = runtime_.RunToCompletion(driver_.PrepareStop());
    EXPECT_EQ(ZX_OK, prepare_stop_result.status_value());
  }

  void DriverStop() {
    zx::result stop_result = driver_.Stop();
    EXPECT_EQ(ZX_OK, stop_result.status_value());
  }

  size_t GetNodeNumber() { return node_server_.SyncCall(&TestNodeLocal::GetchildrenCount); }

  fdf_testing::DriverUnderTest<wlan::iwlwifi::PcieIwlwifiDriver>& driver() { return driver_; }

  async_dispatcher_t* env_dispatcher() { return env_dispatcher_->async_dispatcher(); }

 private:
  // Attaches a foreground dispatcher for us automatically.
  fdf_testing::DriverRuntime runtime_;

  // Env dispatcher runs in the background because we need to make sync calls into it.
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<TestNodeLocal> node_server_{env_dispatcher(), std::in_place,
                                                                  std::string("root")};

  async_patterns::TestDispatcherBound<TestEnvironmentLocal> test_environment_{env_dispatcher(),
                                                                              std::in_place};

  fdf_testing::DriverUnderTest<wlan::iwlwifi::PcieIwlwifiDriver> driver_;

  async_patterns::TestDispatcherBound<FakePciParent> fake_pci_parent_{env_dispatcher(),
                                                                      std::in_place};
};

TEST_F(DriverLifeCycleTest, DeviceLifeCycle) {
  // Starting PcieIwlwifiDriver will trigger AddNode for wlanphy virtual device. Start() hook is
  // called in Setup().
  EXPECT_EQ(GetNodeNumber(), (size_t)1);

  // TODO(b/290283534): Add more operations here like AddWlansoftmacDevice() and
  // RemoveWlansoftmacDevice().

  DriverPrepareStop();
  // The wlanphy child node will be removed by iwlwifi in PrepareStop() hook.
  EXPECT_EQ(GetNodeNumber(), (size_t)0);

  DriverStop();
}

}  // namespace
