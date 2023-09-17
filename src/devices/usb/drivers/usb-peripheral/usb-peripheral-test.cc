// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/usb-peripheral/usb-peripheral.h"

#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <fuchsia/hardware/usb/dci/c/banjo.h>
#include <fuchsia/hardware/usb/dci/cpp/banjo.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>

#include <cstring>
#include <list>
#include <map>
#include <memory>

#include <ddk/usb-peripheral-config.h>
#include <usb/usb.h>
#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/devices/usb/drivers/usb-peripheral/usb-function.h"

class FakeDevice : public ddk::UsbDciProtocol<FakeDevice, ddk::base_protocol> {
 public:
  FakeDevice() : proto_({&usb_dci_protocol_ops_, this}) {}

  // USB DCI protocol implementation.
  void UsbDciRequestQueue(usb_request_t* req, const usb_request_complete_callback_t* cb) {}

  zx_status_t UsbDciSetInterface(const usb_dci_interface_protocol_t* interface) {
    interface_ = *interface;
    return ZX_OK;
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

  usb_dci_protocol_t* proto() { return &proto_; }

  usb_dci_interface_protocol_t* interface() { return &interface_; }

 private:
  usb_dci_interface_protocol_t interface_;
  usb_dci_protocol_t proto_;
};

class UsbPeripheralHarness : public zxtest::Test {
 public:
  void SetUp() override {
    root_device_ = MockDevice::FakeRootParent();
    static const UsbConfig kConfig = []() {
      UsbConfig config = {};
      memcpy(config.serial, kSerialNumber, sizeof(kSerialNumber));
      return config;
    }();
    dci_ = std::make_unique<FakeDevice>();
    root_device_->SetMetadata(DEVICE_METADATA_USB_CONFIG, &kConfig, sizeof(kConfig));
    root_device_->SetMetadata(DEVICE_METADATA_SERIAL_NUMBER, &kSerialNumber, sizeof(kSerialNumber));
    root_device_->AddProtocol(ZX_PROTOCOL_USB_DCI, dci_->proto()->ops, dci_->proto()->ctx);
    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(endpoints);
    root_device_->AddFidlService(fuchsia_hardware_usb_dci::UsbDciService::Name,
                                 std::move(endpoints->client));

    zx::interrupt irq;
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &irq));
    ASSERT_OK(usb_peripheral::UsbPeripheral::Create(nullptr, root_device_.get()));
    ASSERT_EQ(1, root_device_->child_count());
    mock_dev_ = root_device_->GetLatestChild();
    client_ = ddk::UsbDciInterfaceProtocolClient(dci_->interface());
  }

  void TearDown() override {
    mock_dev_->UnbindOp();
    mock_dev_->WaitUntilUnbindReplyCalled();
  }

 protected:
  std::unique_ptr<FakeDevice> dci_;
  std::shared_ptr<MockDevice> root_device_;
  MockDevice* mock_dev_;
  ddk::UsbDciInterfaceProtocolClient client_;

  static constexpr char kSerialNumber[] = "Test serial number";
};

TEST_F(UsbPeripheralHarness, AddsCorrectSerialNumberMetadata) {
  char serial[256];
  usb_setup_t setup;
  setup.w_length = sizeof(serial);
  setup.w_value = 0x3 | (USB_DT_STRING << 8);
  setup.bm_request_type = USB_DIR_IN | USB_RECIP_DEVICE | USB_TYPE_STANDARD;
  setup.b_request = USB_REQ_GET_DESCRIPTOR;
  size_t actual;
  ASSERT_OK(client_.Control(&setup, nullptr, 0, reinterpret_cast<uint8_t*>(&serial), sizeof(serial),
                            &actual));
  ASSERT_EQ(serial[0], sizeof(kSerialNumber) * 2);
  ASSERT_EQ(serial[1], USB_DT_STRING);
  for (size_t i = 0; i < sizeof(kSerialNumber) - 1; i++) {
    ASSERT_EQ(serial[2 + (i * 2)], kSerialNumber[i]);
  }
}

TEST_F(UsbPeripheralHarness, WorksWithVendorSpecificCommandWhenConfigurationIsZero) {
  char serial[256];
  usb_setup_t setup;
  setup.w_length = sizeof(serial);
  setup.w_value = 0x3 | (USB_DT_STRING << 8);
  setup.bm_request_type = USB_DIR_IN | USB_RECIP_DEVICE | USB_TYPE_VENDOR;
  setup.b_request = USB_REQ_GET_DESCRIPTOR;
  size_t actual;
  ASSERT_EQ(client_.Control(&setup, nullptr, 0, reinterpret_cast<uint8_t*>(&serial), sizeof(serial),
                            &actual),
            ZX_ERR_BAD_STATE);
}
