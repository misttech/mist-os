// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.fastboot/cpp/wire_test_base.h>
#include <fuchsia/hardware/usb/function/cpp/banjo-mock.h>

#include <memory>

#include <sdk/lib/inspect/testing/cpp/zxtest/inspect.h>
#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/firmware/drivers/usb-fastboot-function/usb_fastboot_function.h"

// Some test utilities in "fuchsia/hardware/usb/function/cpp/banjo-mock.h" expect the following
// operators to be implemented.

bool operator==(const usb_request_complete_callback_t& lhs,
                const usb_request_complete_callback_t& rhs) {
  // Comparison of these struct is not useful. Return true always.
  return true;
}

bool operator==(const usb_ss_ep_comp_descriptor_t& lhs, const usb_ss_ep_comp_descriptor_t& rhs) {
  // Comparison of these struct is not useful. Return true always.
  return true;
}

bool operator==(const usb_endpoint_descriptor_t& lhs, const usb_endpoint_descriptor_t& rhs) {
  // Comparison of these struct is not useful. Return true always.
  return true;
}

bool operator==(const usb_request_t& lhs, const usb_request_t& rhs) {
  // Only comparing endpoint address. Use ExpectCallWithMatcher for more specific
  // comparisons.
  return lhs.header.ep_address == rhs.header.ep_address;
}

bool operator==(const usb_function_interface_protocol_t& lhs,
                const usb_function_interface_protocol_t& rhs) {
  // Comparison of these struct is not useful. Return true always.
  return true;
}

namespace usb_fastboot_function {
namespace {

static constexpr uint32_t kBulkOutEp = 1;
static constexpr uint32_t kBulkInEp = 2;

using inspect::InspectTestHelper;

class UsbFastbootFunctionTest : public zxtest::Test {
 public:
  UsbFastbootFunctionTest() : loop_(&kAsyncLoopConfigNeverAttachToThread) {}

  ~UsbFastbootFunctionTest() {}
  void SetUp() override {
    fake_root_ = MockDevice::FakeRootParent();
    fake_root_->AddProtocol(ZX_PROTOCOL_USB_FUNCTION, mock_usb_.GetProto()->ops,
                            mock_usb_.GetProto()->ctx);

    // Expect calls from UsbAdbDevice initialization
    mock_usb_.ExpectGetRequestSize(sizeof(usb_request_t));
    mock_usb_.ExpectAllocInterface(ZX_OK, 1);
    mock_usb_.ExpectAllocInterface(ZX_OK, 1);
    mock_usb_.ExpectAllocEp(ZX_OK, USB_DIR_OUT, kBulkOutEp);
    mock_usb_.ExpectAllocEp(ZX_OK, USB_DIR_IN, kBulkInEp);
    mock_usb_.ExpectSetInterface(ZX_OK, {});

    // mock ddk will takes ownership of the pointer.
    device_ = new UsbFastbootFunction(fake_root_.get());
    ASSERT_OK(device_->Bind());
    device_->zxdev()->InitOp();
    ASSERT_OK(device_->zxdev()->WaitUntilInitReplyCalled(zx::time::infinite()));

    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_fastboot::FastbootImpl>();
    ASSERT_OK(endpoints.status_value());
    client_ = fidl::WireSyncClient(std::move(endpoints->client));
    loop_.StartThread("usb-fastboot-function test");
    fidl::BindServer(loop_.dispatcher(), std::move(endpoints->server), device_);
  }

  void TearDown() override {
    device_->DdkAsyncRemove();
    ASSERT_OK(mock_ddk::ReleaseFlaggedDevices(fake_root_.get()));
    mock_usb_.VerifyAndClear();
  }

  ddk::MockUsbFunction& mock_usb() { return mock_usb_; }

  fidl::WireSyncClient<fuchsia_hardware_fastboot::FastbootImpl>& client() { return client_; }

 protected:
  std::shared_ptr<zx_device> fake_root_;
  ddk::MockUsbFunction mock_usb_;
  UsbFastbootFunction* device_ = nullptr;
  async::Loop loop_;
  fidl::WireSyncClient<fuchsia_hardware_fastboot::FastbootImpl> client_;
};

TEST_F(UsbFastbootFunctionTest, LifetimeTest) {
  // Lifetime tested in test Setup() and TearDown()

  // TODO(b/260651258): Implement and move to separate test
  ASSERT_FALSE(client()->Receive(0)->is_ok());

  // TODO(b/260651258): Implement and move to separate test
  fzl::OwnedVmoMapper send_vmo;
  ASSERT_OK(send_vmo.CreateAndMap(32, "test"));
  ASSERT_FALSE(client()->Send(send_vmo.Release())->is_ok());
}

}  // namespace
}  // namespace usb_fastboot_function
