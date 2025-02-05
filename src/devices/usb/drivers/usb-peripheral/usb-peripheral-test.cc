// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-peripheral/usb-peripheral.h"

#include <fidl/fuchsia.hardware.usb.dci/cpp/wire.h>
#include <fidl/fuchsia.hardware.usb.descriptor/cpp/wire.h>
#include <fuchsia/hardware/usb/dci/c/banjo.h>
#include <fuchsia/hardware/usb/dci/cpp/banjo.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>

#include <cstring>
#include <memory>
#include <vector>

#include <usb/usb.h>
#include <zxtest/zxtest.h>

#include "sdk/lib/async_patterns/testing/cpp/dispatcher_bound.h"
#include "sdk/lib/component/outgoing/cpp/outgoing_directory.h"
#include "sdk/lib/driver/testing/cpp/driver_runtime.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/devices/usb/drivers/usb-peripheral/usb-function.h"

namespace fdci = fuchsia_hardware_usb_dci;
namespace fdescriptor = fuchsia_hardware_usb_descriptor;

class FakeDevice : public ddk::UsbDciProtocol<FakeDevice, ddk::base_protocol>,
                   public fidl::WireServer<fdci::UsbDci> {
 public:
  FakeDevice() : proto_({&usb_dci_protocol_ops_, this}) {}

  fdci::UsbDciService::InstanceHandler GetHandler() {
    return fdci::UsbDciService::InstanceHandler(
        {.device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                           fidl::kIgnoreBindingClosure)});
  }

  // USB DCI protocol implementation (No longer used).
  void UsbDciRequestQueue(usb_request_t* req, const usb_request_complete_callback_t* cb) {}
  zx_status_t UsbDciSetInterface(const usb_dci_interface_protocol_t* interface) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t UsbDciConfigEp(const usb_endpoint_descriptor_t* ep_desc,
                             const usb_ss_ep_comp_descriptor_t* ss_comp_desc) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t UsbDciDisableEp(uint8_t ep_address) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t UsbDciEpSetStall(uint8_t ep_address) { return ZX_ERR_NOT_SUPPORTED; }
  zx_status_t UsbDciEpClearStall(uint8_t ep_address) { return ZX_ERR_NOT_SUPPORTED; }
  size_t UsbDciGetRequestSize() { return sizeof(usb_request_t); }

  zx_status_t UsbDciCancelAll(uint8_t ep_address) { return ZX_OK; }

  // fuchsia_hardware_usb_dci::UsbDci protocol.
  void ConnectToEndpoint(ConnectToEndpointRequestView req,
                         ConnectToEndpointCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void SetInterface(SetInterfaceRequestView req, SetInterfaceCompleter::Sync& completer) override {
    fidl::Arena arena;
    client_.Bind(std::move(req->interface));
    completer.buffer(arena).ReplySuccess();
    sync_completion_signal(&set_interface_called_);
  }

  void StartController(StartControllerCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }

  void StopController(StopControllerCompleter::Sync& completer) override {
    completer.ReplySuccess();
  }

  void ConfigureEndpoint(ConfigureEndpointRequestView req,
                         ConfigureEndpointCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void DisableEndpoint(DisableEndpointRequestView req,
                       DisableEndpointCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void EndpointSetStall(EndpointSetStallRequestView req,
                        EndpointSetStallCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void EndpointClearStall(EndpointClearStallRequestView req,
                          EndpointClearStallCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void CancelAll(CancelAllRequestView req, CancelAllCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_usb_dci::UsbDci> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  usb_dci_protocol_t* proto() { return &proto_; }

  fidl::WireSyncClient<fdci::UsbDciInterface>& client() {
    sync_completion_wait(&set_interface_called_, ZX_TIME_INFINITE);
    return client_;
  }

 private:
  usb_dci_protocol_t proto_;
  sync_completion_t set_interface_called_;
  fidl::ServerBindingGroup<fdci::UsbDci> bindings_;
  fidl::WireSyncClient<fdci::UsbDciInterface> client_;
};

class UsbPeripheralHarness : public zxtest::Test {
 public:
  void SetUp() override {
    dci_ = std::make_unique<FakeDevice>();
    root_device_->SetMetadata(DEVICE_METADATA_SERIAL_NUMBER, &kSerialNumber, sizeof(kSerialNumber));
    root_device_->AddProtocol(ZX_PROTOCOL_USB_DCI, dci_->proto()->ops, dci_->proto()->ctx);
    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(endpoints);

    namespace_.SyncCall([&](Namespace* ns) {
      EXPECT_TRUE(ns->outgoing.AddService<fdci::UsbDciService>(ns->dci.GetHandler()).is_ok());
      EXPECT_TRUE(ns->outgoing.Serve(std::move(endpoints->server)).is_ok());
    });

    root_device_->AddFidlService(fdci::UsbDciService::Name, std::move(endpoints->client));

    usb_peripheral_config::Config fake_config;
    fake_config.functions() = {};
    root_device_->SetConfigVmo(fake_config.ToVmo());

    ASSERT_OK(usb_peripheral::UsbPeripheral::Create(nullptr, root_device_.get()));
    ASSERT_EQ(1, root_device_->child_count());
    mock_dev_ = root_device_->GetLatestChild();

    namespace_.SyncCall([&](Namespace* ns) { client_.Bind(ns->dci.client().TakeClientEnd()); });
    ASSERT_TRUE(client_.is_valid());
  }

  // Because this test is using a fidl::WireSyncClient, we need to run any ops on the client on
  // their own thread because the testing thread is shared with the client end.
  static void RunSyncClientTask(fit::closure task) {
    async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
    loop.StartThread();
    zx::result result = fdf::RunOnDispatcherSync(loop.dispatcher(), std::move(task));
    ASSERT_OK(result);
  }

 protected:
  struct Namespace {
    FakeDevice dci;
    component::OutgoingDirectory outgoing{fdf::Dispatcher::GetCurrent()->async_dispatcher()};
  };

  std::unique_ptr<FakeDevice> dci_;
  std::shared_ptr<MockDevice> root_device_{MockDevice::FakeRootParent()};
  MockDevice* mock_dev_;

  fdf::UnownedSynchronizedDispatcher dispatcher_{
      fdf_testing::DriverRuntime::GetInstance()->StartBackgroundDispatcher()};

  async_patterns::TestDispatcherBound<Namespace> namespace_{dispatcher_->async_dispatcher(),
                                                            std::in_place};

  static constexpr char kSerialNumber[] = "Test serial number";

  fidl::WireSyncClient<fdci::UsbDciInterface> client_;
};

TEST_F(UsbPeripheralHarness, AddsCorrectSerialNumberMetadata) {
  fdescriptor::wire::UsbSetup setup;
  setup.w_length = 256;
  setup.w_value = 0x3 | (USB_DT_STRING << 8);
  setup.bm_request_type = USB_DIR_IN | USB_RECIP_DEVICE | USB_TYPE_STANDARD;
  setup.b_request = USB_REQ_GET_DESCRIPTOR;

  fidl::Arena arena;
  RunSyncClientTask([&]() {
    std::vector<uint8_t> unused;
    auto result =
        client_.buffer(arena)->Control(setup, fidl::VectorView<uint8_t>::FromExternal(unused));

    ASSERT_TRUE(result->is_ok());

    auto& serial = result.value()->read;

    EXPECT_EQ(serial[0], sizeof(kSerialNumber) * 2);
    EXPECT_EQ(serial[1], USB_DT_STRING);
    for (size_t i = 0; i < sizeof(kSerialNumber) - 1; i++) {
      EXPECT_EQ(serial[2 + (i * 2)], kSerialNumber[i]);
    }
  });
}

TEST_F(UsbPeripheralHarness, WorksWithVendorSpecificCommandWhenConfigurationIsZero) {
  fdescriptor::wire::UsbSetup setup;
  setup.w_length = 256;
  setup.w_value = 0x3 | (USB_DT_STRING << 8);
  setup.bm_request_type = USB_DIR_IN | USB_RECIP_DEVICE | USB_TYPE_VENDOR;
  setup.b_request = USB_REQ_GET_DESCRIPTOR;

  fidl::Arena arena;
  RunSyncClientTask([&]() {
    std::vector<uint8_t> unused;
    auto result =
        client_.buffer(arena)->Control(setup, fidl::VectorView<uint8_t>::FromExternal(unused));
    ASSERT_TRUE(result->is_error());
    ASSERT_EQ(ZX_ERR_BAD_STATE, result->error_value());
  });
}
