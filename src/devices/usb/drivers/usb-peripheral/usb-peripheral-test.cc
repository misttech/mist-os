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
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>

#include <cstring>
#include <memory>
#include <vector>

#include <gtest/gtest.h>
#include <usb/usb.h>

#include "src/devices/usb/drivers/usb-peripheral/usb-function.h"
#include "src/lib/testing/predicates/status.h"

namespace fdci = fuchsia_hardware_usb_dci;
namespace fdescriptor = fuchsia_hardware_usb_descriptor;

namespace usb_peripheral::test {

class FakeDevice : public ddk::UsbDciProtocol<FakeDevice>, public fidl::WireServer<fdci::UsbDci> {
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

  compat::DeviceServer::BanjoConfig GetBanjoConfig() {
    compat::DeviceServer::BanjoConfig config{ZX_PROTOCOL_USB_DCI};
    config.callbacks[ZX_PROTOCOL_USB_DCI] = banjo_server_.callback();
    return config;
  }

 private:
  usb_dci_protocol_t proto_;
  sync_completion_t set_interface_called_;
  fidl::ServerBindingGroup<fdci::UsbDci> bindings_;
  std::optional<fidl::ClientEnd<fdci::UsbDciInterface>> client_;
  compat::BanjoServer banjo_server_{ZX_PROTOCOL_USB_DCI, this, &usb_dci_protocol_ops_};
};

class UsbPeripheralTestEnvironment : public fdf_testing::Environment {
 public:
  void Init(std::string_view serial_number) {
    fuchsia_boot_metadata::SerialNumberMetadata metadata{{.serial_number{serial_number}}};
    ASSERT_OK(serial_number_metadata_server_.SetMetadata(metadata));

    device_server_.Initialize("default", std::nullopt, dci_.GetBanjoConfig());
  }

  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    if (zx_status_t status =
            device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs);
        status != ZX_OK) {
      return zx::error(status);
    }

    if (zx::result result = serial_number_metadata_server_.Serve(
            to_driver_vfs, fdf::Dispatcher::GetCurrent()->async_dispatcher());
        result.is_error()) {
      return result.take_error();
    }

    if (zx::result result = to_driver_vfs.AddService<fdci::UsbDciService>(dci_.GetHandler());
        result.is_error()) {
      return result.take_error();
    }

    return zx::ok();
  }

  fidl::ClientEnd<fdci::UsbDciInterface> TakeDciClient() { return dci_.TakeClient(); }

 private:
  FakeDevice dci_;
  fdf_metadata::MetadataServer<fuchsia_boot_metadata::SerialNumberMetadata>
      serial_number_metadata_server_;
  compat::DeviceServer device_server_;
};

class UsbPeripheralTestConfig {
 public:
  using DriverType = UsbPeripheral;
  using EnvironmentType = UsbPeripheralTestEnvironment;
};

class UsbPeripheralHarness : public ::testing::Test {
 public:
  void SetUp() override {
    driver_test_.RunInEnvironmentTypeContext([&](auto& env) { env.Init(kSerialNumber); });

    ASSERT_OK(driver_test_.StartDriverWithCustomStartArgs([](auto& start_args) {
      start_args.config().emplace(usb_peripheral_config::Config{}.ToVmo());
    }));

    dci_.Bind(driver_test_.RunInEnvironmentTypeContext<fidl::ClientEnd<fdci::UsbDciInterface>>(
        [](auto& env) { return env.TakeDciClient(); }));
  }

  void TearDown() override { ASSERT_OK(driver_test_.StopDriver()); }

 protected:
  static constexpr std::string_view kSerialNumber = "Test serial number";

  fidl::WireSyncClient<fdci::UsbDciInterface>& dci() { return dci_; }

 private:
  fidl::WireSyncClient<fdci::UsbDciInterface> dci_;
  fdf_testing::BackgroundDriverTest<UsbPeripheralTestConfig> driver_test_;
};

TEST_F(UsbPeripheralHarness, AddsCorrectSerialNumberMetadata) {
  fdescriptor::wire::UsbSetup setup;
  setup.w_length = 256;
  setup.w_value = 0x3 | (USB_DT_STRING << 8);
  setup.bm_request_type = USB_DIR_IN | USB_RECIP_DEVICE | USB_TYPE_STANDARD;
  setup.b_request = USB_REQ_GET_DESCRIPTOR;

  fidl::Arena arena;
  std::vector<uint8_t> unused;
  auto result =
      dci().buffer(arena)->Control(setup, fidl::VectorView<uint8_t>::FromExternal(unused));

  ASSERT_TRUE(result->is_ok());

  auto& serial = result.value()->read;

  EXPECT_EQ(serial[0], (kSerialNumber.size() + 1) * 2);
  EXPECT_EQ(serial[1], USB_DT_STRING);
  for (size_t i = 0; i < sizeof(kSerialNumber) - 1; i++) {
    EXPECT_EQ(serial[2 + (i * 2)], kSerialNumber[i]);
  }
}

TEST_F(UsbPeripheralHarness, WorksWithVendorSpecificCommandWhenConfigurationIsZero) {
  fdescriptor::wire::UsbSetup setup;
  setup.w_length = 256;
  setup.w_value = 0x3 | (USB_DT_STRING << 8);
  setup.bm_request_type = USB_DIR_IN | USB_RECIP_DEVICE | USB_TYPE_VENDOR;
  setup.b_request = USB_REQ_GET_DESCRIPTOR;

  fidl::Arena arena;
  std::vector<uint8_t> unused;
  auto result =
      dci().buffer(arena)->Control(setup, fidl::VectorView<uint8_t>::FromExternal(unused));
  ASSERT_TRUE(result->is_error());
  ASSERT_EQ(ZX_ERR_BAD_STATE, result->error_value());
}

}  // namespace usb_peripheral::test
