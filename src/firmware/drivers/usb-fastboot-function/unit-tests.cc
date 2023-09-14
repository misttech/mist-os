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

class MockFastbootUsbFunction : public ddk::MockUsbFunction {
 public:
  ~MockFastbootUsbFunction() { Join(); }

  // Override ConfigEp to be a no-op. Because we pass nullptr as the second argument. But test
  // implementation tries to dereference it.
  zx_status_t UsbFunctionConfigEp(const usb_endpoint_descriptor_t*,
                                  const usb_ss_ep_comp_descriptor_t*) override {
    return ZX_OK;
  }

  void UsbFunctionRequestQueue(usb_request_t* usb_request,
                               const usb_request_complete_callback_t* complete_cb) override {
    mock_request_queue_.Call(usb_request, complete_cb);
    // Callback needs to be invoked in a different thread. Otherwise it tries to re-acquire the
    // mutex and fails the test.
    cb_threads_.push_back(std::thread(
        [complete_cb, usb_request]() { complete_cb->callback(complete_cb->ctx, usb_request); }));
  }

  void Join() {
    for (auto& t : cb_threads_) {
      t.join();
    }
    cb_threads_.clear();
  }

  using MockRequestQueue =
      mock_function::MockFunction<void, usb_request_t*, const usb_request_complete_callback_t*>;

  MockRequestQueue& mock_request_queue() { return mock_request_queue_; }

 private:
  std::vector<std::thread> cb_threads_;
  MockRequestQueue mock_request_queue_;
};

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
    mock_usb_.Join();
    device_->DdkAsyncRemove();
    loop_.Shutdown();
    fake_root_ = nullptr;
    mock_usb_.VerifyAndClear();
  }

  MockFastbootUsbFunction& mock_usb() { return mock_usb_; }
  fidl::WireSyncClient<fuchsia_hardware_fastboot::FastbootImpl>& client() { return client_; }
  UsbFastbootFunction* device() { return device_; }

 protected:
  std::shared_ptr<zx_device> fake_root_;
  MockFastbootUsbFunction mock_usb_;
  UsbFastbootFunction* device_ = nullptr;
  async::Loop loop_;
  fidl::WireSyncClient<fuchsia_hardware_fastboot::FastbootImpl> client_;
};

TEST_F(UsbFastbootFunctionTest, LifetimeTest) {
  // Lifetime tested in test Setup() and TearDown()
}

void ValidateVmo(const zx::vmo& vmo, std::string_view payload) {
  fzl::VmoMapper mapper;
  ASSERT_OK(mapper.Map(vmo));
  size_t content_size = 0;
  ASSERT_OK(vmo.get_prop_content_size(&content_size));
  ASSERT_EQ(content_size, payload.size());
  ASSERT_EQ(memcmp(mapper.start(), payload.data(), payload.size()), 0);
}

TEST_F(UsbFastbootFunctionTest, ReceiveTestSinglePacket) {
  device()->UsbFunctionInterfaceSetConfigured(true, 0);
  const std::string_view test_data = "getvar:all";
  mock_usb().mock_request_queue().ExpectCallWithMatcher(
      [&](usb_request_t* req, const usb_request_complete_callback_t* complete_cb) -> void {
        ASSERT_EQ(req->header.ep_address, kBulkOutEp);
        ASSERT_GT(usb_request_copy_to(req, test_data.data(), test_data.size(), 0), 0);
        req->response.status = ZX_OK;
        req->response.actual = test_data.size();
      });
  auto res = client()->Receive(0);
  ASSERT_TRUE(res->is_ok());
  ValidateVmo(res.value()->data, test_data);
}

TEST_F(UsbFastbootFunctionTest, ReceiveStateReset) {
  device()->UsbFunctionInterfaceSetConfigured(true, 0);
  const std::string_view test_data = "getvar:all";
  auto packet_matcher = [&](usb_request_t* req,
                            const usb_request_complete_callback_t* complete_cb) -> void {
    ASSERT_EQ(req->header.ep_address, kBulkOutEp);
    ASSERT_GT(usb_request_copy_to(req, test_data.data(), test_data.size(), 0), 0);
    req->response.status = ZX_OK;
    req->response.actual = test_data.size();
  };

  {
    mock_usb().mock_request_queue().ExpectCallWithMatcher(packet_matcher);
    auto res = client()->Receive(0);
    ASSERT_TRUE(res->is_ok());
    ValidateVmo(res.value()->data, test_data);
  }

  // Repetitive call to receive should continue to work. This is to test that internal state and
  // variables that tracks caller completer and remaining receive size are properly reset for next
  // Receive.
  {
    mock_usb().mock_request_queue().ExpectCallWithMatcher(packet_matcher);
    auto res = client()->Receive(0);
    ASSERT_TRUE(res->is_ok());
    ValidateVmo(res.value()->data, test_data);
  }
}

TEST_F(UsbFastbootFunctionTest, ReceiveTestMultiplePacket) {
  device()->UsbFunctionInterfaceSetConfigured(true, 0);
  const std::string_view expected = "getvar:all";
  // First packet contains "getvar"
  mock_usb().mock_request_queue().ExpectCallWithMatcher(
      [&](usb_request_t* req, const usb_request_complete_callback_t* complete_cb) -> void {
        ASSERT_EQ(req->header.ep_address, kBulkOutEp);
        const char segment[] = "getvar";
        ASSERT_GT(usb_request_copy_to(req, segment, strlen(segment), 0), 0);
        req->response.status = ZX_OK;
        req->response.actual = strlen(segment);
      });

  // Second packet is an error
  mock_usb().mock_request_queue().ExpectCallWithMatcher(
      [&](usb_request_t* req, const usb_request_complete_callback_t* complete_cb) -> void {
        ASSERT_EQ(req->header.ep_address, kBulkOutEp);
        req->response.status = ZX_ERR_INTERNAL;  // shouldn't affect the result.
      });

  // Third packet is ":all"
  mock_usb().mock_request_queue().ExpectCallWithMatcher(
      [&](usb_request_t* req, const usb_request_complete_callback_t* complete_cb) -> void {
        ASSERT_EQ(req->header.ep_address, kBulkOutEp);
        const char segment[] = ":all";
        ASSERT_GT(usb_request_copy_to(req, segment, strlen(segment), 0), 0);
        req->response.status = ZX_OK;
        req->response.actual = strlen(segment);
      });
  auto res = client()->Receive(expected.size());
  ASSERT_TRUE(res->is_ok());
  ValidateVmo(res.value()->data, expected);
}

TEST_F(UsbFastbootFunctionTest, ReceiveFailsOnIoNotPresentTest) {
  device()->UsbFunctionInterfaceSetConfigured(true, 0);
  mock_usb().mock_request_queue().ExpectCallWithMatcher(
      [&](usb_request_t* req, const usb_request_complete_callback_t* complete_cb) -> void {
        ASSERT_EQ(req->header.ep_address, kBulkOutEp);
        req->response.status = ZX_ERR_IO_NOT_PRESENT;
      });
  auto res = client()->Receive(0);
  ASSERT_FALSE(res->is_ok());
}

TEST_F(UsbFastbootFunctionTest, ReceiveFailsOnNonConfiguredInterface) {
  mock_usb().ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb().ExpectDisableEp(ZX_OK, kBulkInEp);
  device_->UsbFunctionInterfaceSetConfigured(false, 0);
  ASSERT_FALSE(client()->Receive(0)->is_ok());
}

void InitializeSendVmo(fzl::OwnedVmoMapper& vmo, std::string_view data) {
  ASSERT_OK(vmo.CreateAndMap(data.size(), "test"));
  ASSERT_OK(vmo.vmo().set_prop_content_size(data.size()));
  memcpy(vmo.start(), data.data(), data.size());
}

TEST_F(UsbFastbootFunctionTest, Send) {
  device()->UsbFunctionInterfaceSetConfigured(true, 0);
  const std::string_view send_data = "OKAY0.4";

  mock_usb().mock_request_queue().ExpectCallWithMatcher(
      [&](usb_request_t* req, const usb_request_complete_callback_t* complete_cb) -> void {
        ASSERT_EQ(req->header.ep_address, kBulkInEp);
        char req_data[sizeof(send_data)];
        ASSERT_GT(usb_request_copy_from(req, req_data, send_data.size(), 0), 0);
        req->response.status = ZX_OK;
        ASSERT_EQ(memcmp(send_data.data(), req_data, send_data.size()), 0);
      });
  fzl::OwnedVmoMapper send_vmo;
  InitializeSendVmo(send_vmo, send_data);
  ASSERT_TRUE(client()->Send(send_vmo.Release())->is_ok());
}

TEST_F(UsbFastbootFunctionTest, SendStatesReset) {
  device()->UsbFunctionInterfaceSetConfigured(true, 0);
  const std::string_view send_data = "OKAY0.4";
  auto matcher = [&](usb_request_t* req,
                     const usb_request_complete_callback_t* complete_cb) -> void {
    ASSERT_EQ(req->header.ep_address, kBulkInEp);
    char req_data[sizeof(send_data)];
    ASSERT_GT(usb_request_copy_from(req, req_data, send_data.size(), 0), 0);
    req->response.status = ZX_OK;
    ASSERT_EQ(memcmp(send_data.data(), req_data, send_data.size()), 0);
  };

  {
    mock_usb().mock_request_queue().ExpectCallWithMatcher(matcher);
    fzl::OwnedVmoMapper send_vmo;
    InitializeSendVmo(send_vmo, send_data);
    ASSERT_TRUE(client()->Send(send_vmo.Release())->is_ok());
  }

  // Repetitve call to Send() should continue to work. This is to test that internal
  // state/variables tracking caller completer and send size are properly reset.
  {
    mock_usb().mock_request_queue().ExpectCallWithMatcher(matcher);
    fzl::OwnedVmoMapper send_vmo;
    InitializeSendVmo(send_vmo, send_data);
    ASSERT_TRUE(client()->Send(send_vmo.Release())->is_ok());
  }
}

TEST_F(UsbFastbootFunctionTest, SendMultiplePacketTest) {
  device()->UsbFunctionInterfaceSetConfigured(true, 0);
  // Create a data that needs two full USB packets to send.
  std::string first(kBulkReqSize, 'A'), second(kBulkReqSize, 'B');
  std::string send = first + second;

  // First packet sends the first kBulkReqSize bytes
  mock_usb().mock_request_queue().ExpectCallWithMatcher(
      [&](usb_request_t* req, const usb_request_complete_callback_t* complete_cb) -> void {
        ASSERT_EQ(req->header.ep_address, kBulkInEp);
        char req_data[kBulkReqSize];
        ASSERT_GT(usb_request_copy_from(req, req_data, kBulkReqSize, 0), 0);
        req->response.status = ZX_OK;
        ASSERT_EQ(memcmp(first.data(), req_data, kBulkReqSize), 0);
      });

  // Second packet is an error.
  mock_usb().mock_request_queue().ExpectCallWithMatcher(
      [&](usb_request_t* req, const usb_request_complete_callback_t* complete_cb) -> void {
        ASSERT_EQ(req->header.ep_address, kBulkInEp);
        req->response.status = ZX_ERR_INTERNAL;  // Should retry.
      });

  // Third packet sends the second kBulkReqSize bytes
  mock_usb().mock_request_queue().ExpectCallWithMatcher(
      [&](usb_request_t* req, const usb_request_complete_callback_t* complete_cb) -> void {
        ASSERT_EQ(req->header.ep_address, kBulkInEp);
        char req_data[kBulkReqSize];
        ASSERT_GT(usb_request_copy_from(req, req_data, kBulkReqSize, 0), 0);
        req->response.status = ZX_OK;
        ASSERT_EQ(memcmp(second.data(), req_data, kBulkReqSize), 0);
      });

  fzl::OwnedVmoMapper send_vmo;
  InitializeSendVmo(send_vmo, send);
  ASSERT_TRUE(client()->Send(send_vmo.Release())->is_ok());
}

TEST_F(UsbFastbootFunctionTest, SendFailOnIoNotPresent) {
  device()->UsbFunctionInterfaceSetConfigured(true, 0);
  const std::string_view send_data = "OKAY0.4";
  mock_usb().mock_request_queue().ExpectCallWithMatcher(
      [&](usb_request_t* req, const usb_request_complete_callback_t* complete_cb) -> void {
        ASSERT_EQ(req->header.ep_address, kBulkInEp);
        req->response.status = ZX_ERR_IO_NOT_PRESENT;  // Should retry.
      });

  fzl::OwnedVmoMapper send_vmo;
  InitializeSendVmo(send_vmo, send_data);
  ASSERT_FALSE(client()->Send(send_vmo.Release())->is_ok());
}

TEST_F(UsbFastbootFunctionTest, SendFailsOnNonConfiguredInterface) {
  mock_usb().ExpectDisableEp(ZX_OK, kBulkOutEp);
  mock_usb().ExpectDisableEp(ZX_OK, kBulkInEp);
  device_->UsbFunctionInterfaceSetConfigured(false, 0);

  const std::string_view send_data = "OKAY0.4";
  fzl::OwnedVmoMapper send_vmo;
  InitializeSendVmo(send_vmo, send_data);
  ASSERT_FALSE(client()->Send(send_vmo.Release())->is_ok());
}

TEST_F(UsbFastbootFunctionTest, SendFailsOnZeroContentSize) {
  device_->UsbFunctionInterfaceSetConfigured(true, 0);
  const std::string_view send_data = "OKAY0.4";
  fzl::OwnedVmoMapper send_vmo;
  InitializeSendVmo(send_vmo, send_data);
  ASSERT_OK(send_vmo.vmo().set_prop_content_size(0));
  ASSERT_FALSE(client()->Send(send_vmo.Release())->is_ok());
}

}  // namespace
}  // namespace usb_fastboot_function
