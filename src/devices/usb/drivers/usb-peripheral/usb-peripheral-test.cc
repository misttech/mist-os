// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-peripheral/usb-peripheral.h"

#include <fidl/fuchsia.hardware.usb.dci/cpp/wire.h>
#include <fidl/fuchsia.hardware.usb.descriptor/cpp/wire.h>
#include <fuchsia/hardware/usb/dci/c/banjo.h>
#include <fuchsia/hardware/usb/dci/cpp/banjo.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>

#include <cstring>
#include <memory>
#include <vector>

#include <gtest/gtest.h>
#include <usb/usb.h>

#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/devices/usb/drivers/usb-peripheral/usb-function.h"
#include "src/lib/testing/predicates/status.h"

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
    client_.emplace(std::move(req->interface));
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

  fidl::ClientEnd<fdci::UsbDciInterface> TakeClient() {
    sync_completion_wait(&set_interface_called_, ZX_TIME_INFINITE);
    auto client = std::move(client_);
    EXPECT_TRUE(client.has_value());
    return std::move(client.value());
  }

 private:
  usb_dci_protocol_t proto_;
  sync_completion_t set_interface_called_;
  fidl::ServerBindingGroup<fdci::UsbDci> bindings_;
  std::optional<fidl::ClientEnd<fdci::UsbDciInterface>> client_;
};

class UsbPeripheralTestEnvironment {
 public:
  void Init(std::string_view serial_number, fidl::ServerEnd<fuchsia_io::Directory> dci_service,
            fidl::ServerEnd<fuchsia_io::Directory> serial_number_service) {
    fuchsia_boot_metadata::SerialNumberMetadata metadata{{.serial_number{serial_number}}};
    ASSERT_OK(serial_number_metadata_server_.SetMetadata(metadata));
    ASSERT_OK(serial_number_metadata_server_.Serve(outgoing_, dispatcher_));
    ASSERT_OK(outgoing_.Serve(std::move(serial_number_service)));

    ASSERT_OK(outgoing_.AddService<fdci::UsbDciService>(dci_.GetHandler()));
    ASSERT_OK(outgoing_.Serve(std::move(dci_service)));
  }

  fidl::ClientEnd<fdci::UsbDciInterface> TakeDciClient() { return dci_.TakeClient(); }

 private:
  async_dispatcher_t* dispatcher_ = fdf::Dispatcher::GetCurrent()->async_dispatcher();
  component::OutgoingDirectory outgoing_{dispatcher_};
  FakeDevice dci_;
  ddk::MetadataServer<fuchsia_boot_metadata::SerialNumberMetadata> serial_number_metadata_server_;
};

class UsbPeripheralHarness : public ::testing::Test {
 public:
  void SetUp() override {
    dci_ = std::make_unique<FakeDevice>();
    root_device_->AddProtocol(ZX_PROTOCOL_USB_DCI, dci_->proto()->ops, dci_->proto()->ctx);

    auto [dci_service_client, dci_service_server] =
        fidl::Endpoints<fuchsia_io::Directory>::Create();
    auto [serial_number_service_client, serial_number_service_server] =
        fidl::Endpoints<fuchsia_io::Directory>::Create();
    environment_.SyncCall(&UsbPeripheralTestEnvironment::Init, kSerialNumber,
                          std::move(dci_service_server), std::move(serial_number_service_server));
    root_device_->AddFidlService(fdci::UsbDciService::Name, std::move(dci_service_client));
    root_device_->AddFidlService(
        ddk::MetadataServer<fuchsia_boot_metadata::SerialNumberMetadata>::kFidlServiceName,
        std::move(serial_number_service_client), "default");

    usb_peripheral_config::Config fake_config;
    fake_config.functions() = {};
    root_device_->SetConfigVmo(fake_config.ToVmo());

    ASSERT_OK(usb_peripheral::UsbPeripheral::Create(nullptr, root_device_.get()));
    ASSERT_EQ(1u, root_device_->child_count());
    mock_dev_ = root_device_->GetLatestChild();

    client_.Bind(environment_.SyncCall(&UsbPeripheralTestEnvironment::TakeDciClient));
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
  std::unique_ptr<FakeDevice> dci_;
  std::shared_ptr<MockDevice> root_device_{MockDevice::FakeRootParent()};
  MockDevice* mock_dev_;

  fdf::UnownedSynchronizedDispatcher dispatcher_{
      fdf_testing::DriverRuntime::GetInstance()->StartBackgroundDispatcher()};

  async_patterns::TestDispatcherBound<UsbPeripheralTestEnvironment> environment_{
      dispatcher_->async_dispatcher(), std::in_place};

  static constexpr std::string_view kSerialNumber = "Test serial number";

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

    EXPECT_EQ(serial[0], (kSerialNumber.size() +1) * 2);
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
