// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "overnet_usb.h"

#include <fuchsia/hardware/usb/function/cpp/banjo.h>
#include <lib/ddk/metadata.h>
#include <lib/sync/completion.h>
#include <zircon/compiler.h>

#include <algorithm>
#include <cstdint>
#include <memory>
#include <optional>
#include <string_view>
#include <vector>

#include <zxtest/zxtest.h>

#include "ddktl/unbind-txn.h"
#include "fbl/auto_lock.h"
#include "fbl/condition_variable.h"
#include "fbl/mutex.h"
#include "fidl/fuchsia.hardware.overnet/cpp/markers.h"
#include "lib/async-loop/loop.h"
#include "lib/fidl/cpp/wire/channel.h"
#include "lib/fidl/cpp/wire/internal/transport_channel.h"
#include "lib/zx/channel.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "usb/request-cpp.h"

// NOLINTBEGIN(misc-use-anonymous-namespace)
// NOLINTBEGIN(readability-convert-member-functions-to-static)
// NOLINTBEGIN(readability-container-data-pointer)

static constexpr uint8_t kOvernetMagic[] = "OVERNET USB\xff\x00\xff\x00\xff";
static constexpr size_t kOvernetMagicSize = sizeof(kOvernetMagic) - 1;

class FakeFunction : public ddk::UsbFunctionProtocol<FakeFunction, ddk::base_protocol> {
 public:
  FakeFunction() : protocol_({.ops = &usb_function_protocol_ops_, .ctx = this}) {}
  ~FakeFunction() {
    fbl::AutoLock lock(&lock_);
    shutting_down_ = true;
    lock.release();
    pending_in_requests_.CompleteAll(ZX_ERR_CANCELED, 0);
    pending_out_requests_.CompleteAll(ZX_ERR_CANCELED, 0);
  }

  zx_status_t UsbFunctionSetInterface(const usb_function_interface_protocol_t* interface) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t UsbFunctionAllocInterface(uint8_t* out_intf_num) { return ZX_OK; }

  zx_status_t UsbFunctionAllocEp(uint8_t direction, uint8_t* out_address) {
    if (direction == USB_DIR_OUT) {
      *out_address = kBulkOutEndpoint;
    } else if (direction == USB_DIR_IN) {
      // This unfortunately relies on the order of endpoint allocation in the driver.
      *out_address = kBulkInEndpoint;
    } else {
      return ZX_ERR_BAD_STATE;
    }
    return ZX_OK;
  }

  zx_status_t UsbFunctionConfigEp(const usb_endpoint_descriptor_t* ep_desc,
                                  const usb_ss_ep_comp_descriptor_t* ss_comp_desc) {
    config_ep_calls_ += 1;
    return ZX_OK;
  }

  zx_status_t UsbFunctionDisableEp(uint8_t address) {
    disable_ep_calls_ += 1;
    return ZX_OK;
  }

  zx_status_t UsbFunctionAllocStringDesc(const char* str, uint8_t* out_index) { return ZX_OK; }

  void UsbFunctionRequestQueue(usb_request_t* usb_request,
                               const usb_request_complete_callback_t* complete_cb) {
    fbl::AutoLock lock(&lock_);
    usb::BorrowedRequest request(usb_request, *complete_cb, sizeof(usb_request_t));
    if (shutting_down_) {
      request.Complete(ZX_ERR_CANCELED, 0);
      return;
    }
    switch (request.request()->header.ep_address) {
      case kBulkOutEndpoint: {
        pending_out_requests_.push(std::move(request));
        out_requests_count_ += 1;
        count_signal_.Broadcast();
      } break;
      case kBulkInEndpoint: {
        pending_in_requests_.push(std::move(request));
        in_requests_count_ += 1;
        count_signal_.Broadcast();
      } break;
      default:
        request.Complete(ZX_ERR_INVALID_ARGS, 0);
        break;
    }
  }

  zx_status_t UsbFunctionEpSetStall(uint8_t ep_address) { return ZX_ERR_NOT_SUPPORTED; }

  zx_status_t UsbFunctionEpClearStall(uint8_t ep_address) { return ZX_ERR_NOT_SUPPORTED; }

  size_t UsbFunctionGetRequestSize() {
    return usb::BorrowedRequest<>::RequestSize(sizeof(usb_request_t));
  }

  zx_status_t SetConfigured(bool configured, usb_speed_t speed) { return ZX_ERR_NOT_SUPPORTED; }

  // NOLINTNEXTLINE(bugprone-easily-swappable-parameters)
  zx_status_t SetInterface(uint8_t interface, uint8_t alt_setting) { return ZX_ERR_NOT_SUPPORTED; }

  zx_status_t Control(const usb_setup_t* setup, const void* write_buffer, size_t write_size,
                      void* read_buffer, size_t read_size, size_t* out_read_actual) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t UsbFunctionCancelAll(uint8_t ep_address) {
    fbl::AutoLock lock(&lock_);
    if (shutting_down_) {
      return ZX_OK;
    }
    switch (ep_address) {
      case kBulkOutEndpoint:
        if (!pending_out_requests_.is_empty()) {
          pending_out_requests_.CompleteAll(ZX_ERR_IO_NOT_PRESENT, 0);
          out_requests_count_ = 0;
          count_signal_.Broadcast();
        }
        break;
      case kBulkInEndpoint:
        if (!pending_in_requests_.is_empty()) {
          pending_in_requests_.CompleteAll(ZX_ERR_IO_NOT_PRESENT, 0);
          in_requests_count_ = 0;
          count_signal_.Broadcast();
        }
        break;
      default:
        return ZX_ERR_INVALID_ARGS;
    }
    return ZX_OK;
  }

  void WaitForPendingIn() {
    fbl::AutoLock lock(&lock_);
    while (in_requests_count_ == 0) {
      count_signal_.Wait(&lock_);
    }
  }

  void WaitForPendingOut() {
    fbl::AutoLock lock(&lock_);
    while (out_requests_count_ == 0) {
      count_signal_.Wait(&lock_);
    }
  }

  std::optional<usb::BorrowedRequest<>> GetOutRequest() {
    fbl::AutoLock lock(&lock_);
    if (out_requests_count_ > 0) {
      out_requests_count_ -= 1;
    }
    return pending_out_requests_.pop();
  }

  std::optional<usb::BorrowedRequest<>> GetInRequest() {
    fbl::AutoLock lock(&lock_);
    if (in_requests_count_ > 0) {
      in_requests_count_ -= 1;
    }
    return pending_in_requests_.pop();
  }

  const usb_function_protocol_t* proto() const { return &protocol_; }

  size_t ConfigEpCalls() const { return config_ep_calls_; }
  size_t DisableEpCalls() const { return disable_ep_calls_; }

 private:
  fbl::Mutex lock_;
  bool shutting_down_ __TA_GUARDED(lock_) = false;
  fbl::ConditionVariable count_signal_ __TA_GUARDED(lock_);
  usb::BorrowedRequestQueue<void> pending_out_requests_ __TA_GUARDED(lock_);
  usb::BorrowedRequestQueue<void> pending_in_requests_ __TA_GUARDED(lock_);
  size_t out_requests_count_ __TA_GUARDED(lock_) = 0;
  size_t in_requests_count_ __TA_GUARDED(lock_) = 0;

  static constexpr uint8_t kBulkOutEndpoint = 0;
  static constexpr uint8_t kBulkInEndpoint = 1;

  usb_function_protocol_t protocol_;

  size_t config_ep_calls_ = 0;
  size_t disable_ep_calls_ = 0;
};

class TestCallback : public fidl::WireServer<fuchsia_hardware_overnet::Callback> {
 public:
  void NewLink(::fuchsia_hardware_overnet::wire::CallbackNewLinkRequest* request,
               NewLinkCompleter::Sync& completer) override {
    fbl::AutoLock lock(&lock_);
    sockets_.emplace_back(std::move(request->socket));
    sockets_changed_.Broadcast();
    completer.Reply();
  }

  void WaitForSockets(size_t count) {
    fbl::AutoLock lock(&lock_);

    while (sockets_.size() < count) {
      sockets_changed_.Wait(&lock_);
    }
  }

  fbl::Mutex lock_;
  fbl::ConditionVariable sockets_changed_;
  std::vector<zx::socket> sockets_;
};

class OvernetUsbTest : public zxtest::Test {
 public:
  void SetUp() override {
    fake_parent_ = MockDevice::FakeRootParent();
    fake_parent_->AddProtocol(ZX_PROTOCOL_USB_FUNCTION, function_.proto()->ops,
                              function_.proto()->ctx);

    device_ = std::make_unique<OvernetUsb>(fake_parent_.get());
    device_->Bind();
    ASSERT_EQ(fake_parent_->child_count(), 1);

    auto endpoints = fidl::Endpoints<fuchsia_hardware_overnet::Device>::Create();
    ASSERT_OK(fidl_loop_.StartThread("usb-adb-test-loop"));
    fidl::BindServer(fidl_loop_.dispatcher(), std::move(endpoints.server), device_.get());
    client_ = fidl::WireSyncClient<fuchsia_hardware_overnet::Device>(std::move(endpoints.client));
  }

  std::unique_ptr<TestCallback> SetTestCallback() const {
    auto ret = std::make_unique<TestCallback>();
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_overnet::Callback>();
    if (!endpoints.is_ok()) {
      return nullptr;
    }
    fidl::BindServer(fidl_loop_.dispatcher(), std::move(endpoints->server), ret.get());
    if (!client_->SetCallback(std::move(endpoints->client)).ok()) {
      return nullptr;
    }
    return ret;
  }

  bool SendMagic() { return SendTx(kOvernetMagic, kOvernetMagicSize); }

  bool GetMagic() {
    auto got = GetRx();

    if (!got) {
      return false;
    }

    if (got->size() != kOvernetMagicSize) {
      return false;
    }

    return std::equal(kOvernetMagic, kOvernetMagic + kOvernetMagicSize, got->begin());
  }

  bool SendTx(const uint8_t* tx, size_t size) {
    function_.WaitForPendingOut();
    auto req = function_.GetOutRequest();

    if (!req) {
      return false;
    }

    auto request = std::move(req.value());

    if (request.request()->header.length < size) {
      return false;
    }

    uint8_t* data;

    if (request.Mmap(reinterpret_cast<void**>(&data)) != ZX_OK) {
      return false;
    }

    std::copy(tx, tx + size, data);
    request.Complete(ZX_OK, size);

    return true;
  }

  std::optional<std::vector<uint8_t>> GetRx() {
    function_.WaitForPendingIn();
    auto request = function_.GetInRequest();

    if (!request) {
      return std::nullopt;
    }

    uint8_t* data;

    if (request->Mmap(reinterpret_cast<void**>(&data)) != ZX_OK) {
      return std::nullopt;
    }

    auto length = request->request()->header.length;
    std::vector<uint8_t> ret(data, data + length);

    request->Complete(ZX_OK, length);
    return std::move(ret);
  }

  bool SocketReadExpect(zx::socket* socket, const uint8_t* data, size_t len) {
    std::vector<uint8_t> buf(len, 0);

    while (len > 0) {
      zx_signals_t pending;
      size_t actual;
      if (socket->wait_one(ZX_SOCKET_READABLE, zx::time::infinite(), &pending) != ZX_OK) {
        return false;
      }
      if ((pending & ZX_SOCKET_READABLE) == 0) {
        return false;
      }
      if (socket->read(0, buf.data(), len, &actual) != ZX_OK) {
        return false;
      }
      if (!std::equal(buf.begin(), buf.begin() + static_cast<ssize_t>(actual), data)) {
        return false;
      }
      len -= actual;
      data += actual;
    }
    return true;
  }

  bool SocketWriteAll(zx::socket* socket, const uint8_t* data, size_t len) {
    while (len > 0) {
      zx_signals_t pending;
      size_t actual;
      if (socket->wait_one(ZX_SOCKET_WRITABLE, zx::time::infinite(), &pending) != ZX_OK) {
        return false;
      }
      if ((pending & ZX_SOCKET_WRITABLE) == 0) {
        return false;
      }
      if (socket->write(0, data, len, &actual) != ZX_OK) {
        return false;
      }
      len -= actual;
      data += actual;
    }
    return true;
  }

  bool GetRxConcatExpect(const uint8_t* data, size_t len) {
    while (len != 0) {
      auto got = GetRx();
      if (!got.has_value()) {
        return false;
      }
      if (got->size() > len) {
        return false;
      }
      if (!std::equal(got->begin(), got->end(), data)) {
        return false;
      }
      len -= got->size();
      data += got->size();
    }
    return true;
  }

  void TearDown() override {
    // Owned by the ddk itself now.
    fidl_loop_.Shutdown();
    (void)device_.release();
    auto dev = fake_parent_->GetLatestChild();
    dev->UnbindOp();
    ASSERT_OK(dev->WaitUntilUnbindReplyCalled());
  }

  std::shared_ptr<MockDevice> fake_parent_;
  async::Loop fidl_loop_{&kAsyncLoopConfigNeverAttachToThread};
  fidl::WireSyncClient<fuchsia_hardware_overnet::Device> client_;
  std::unique_ptr<OvernetUsb> device_;
  FakeFunction function_;
};

TEST_F(OvernetUsbTest, Suspend) {
  auto dev = fake_parent_->GetLatestChild();
  dev->SuspendNewOp(DEV_POWER_STATE_D0, false, DEVICE_SUSPEND_REASON_SUSPEND_RAM);
  ASSERT_OK(dev->WaitUntilSuspendReplyCalled());
}

TEST_F(OvernetUsbTest, Configure) {
  EXPECT_EQ(function_.ConfigEpCalls(), 0);
  EXPECT_EQ(function_.DisableEpCalls(), 0);

  zx_status_t status =
      device_->UsbFunctionInterfaceSetConfigured(/*configured=*/true, USB_SPEED_FULL);
  ASSERT_OK(status);

  EXPECT_EQ(function_.ConfigEpCalls(), 2);
  EXPECT_EQ(function_.DisableEpCalls(), 0);

  status = device_->UsbFunctionInterfaceSetConfigured(/*configured=*/false, USB_SPEED_FULL);
  ASSERT_OK(status);

  EXPECT_EQ(function_.ConfigEpCalls(), 2);
  EXPECT_EQ(function_.DisableEpCalls(), 2);
}

TEST_F(OvernetUsbTest, SocketGet) {
  zx_status_t status =
      device_->UsbFunctionInterfaceSetConfigured(/*configured=*/true, USB_SPEED_FULL);
  ASSERT_OK(status);
  auto callback = SetTestCallback();
  ASSERT_NOT_NULL(callback);
  ASSERT_TRUE(SendMagic());
  callback->WaitForSockets(1);
  ASSERT_TRUE(GetMagic());
}

TEST_F(OvernetUsbTest, DataFromTarget) {
  zx_status_t status =
      device_->UsbFunctionInterfaceSetConfigured(/*configured=*/true, USB_SPEED_FULL);
  ASSERT_OK(status);
  auto callback = SetTestCallback();
  ASSERT_NOT_NULL(callback);
  ASSERT_TRUE(SendMagic());
  callback->WaitForSockets(1);
  ASSERT_TRUE(GetMagic());

  std::string_view test_data =
      "A basket of biscuits, a basket of mixed biscuits and a biscuit mixer.";
  ASSERT_TRUE(SocketWriteAll(&callback->sockets_[0],
                             reinterpret_cast<const uint8_t*>(test_data.data()), test_data.size()));
  ASSERT_TRUE(
      GetRxConcatExpect(reinterpret_cast<const uint8_t*>(test_data.data()), test_data.size()));

  std::string_view test_data_b = "Aluminum, linoleum, magnesium, petroleum.";
  ASSERT_TRUE(SocketWriteAll(&callback->sockets_[0],
                             reinterpret_cast<const uint8_t*>(test_data_b.data()),
                             test_data_b.size()));
  ASSERT_TRUE(
      GetRxConcatExpect(reinterpret_cast<const uint8_t*>(test_data_b.data()), test_data_b.size()));
}

TEST_F(OvernetUsbTest, DataFromHost) {
  zx_status_t status =
      device_->UsbFunctionInterfaceSetConfigured(/*configured=*/true, USB_SPEED_FULL);
  ASSERT_OK(status);
  auto callback = SetTestCallback();
  ASSERT_NOT_NULL(callback);
  ASSERT_TRUE(SendMagic());
  callback->WaitForSockets(1);
  ASSERT_TRUE(GetMagic());

  std::string_view test_data =
      "A basket of biscuits, a basket of mixed biscuits and a biscuit mixer.";
  ASSERT_TRUE(SendTx(reinterpret_cast<const uint8_t*>(test_data.data()), test_data.size()));
  ASSERT_TRUE(SocketReadExpect(&callback->sockets_[0],
                               reinterpret_cast<const uint8_t*>(test_data.data()),
                               test_data.size()));

  std::string_view test_data_b = "Aluminum, linoleum, magnesium, petroleum.";
  ASSERT_TRUE(SendTx(reinterpret_cast<const uint8_t*>(test_data_b.data()), test_data_b.size()));
  ASSERT_TRUE(SocketReadExpect(&callback->sockets_[0],
                               reinterpret_cast<const uint8_t*>(test_data_b.data()),
                               test_data_b.size()));
}

TEST_F(OvernetUsbTest, Reset) {
  zx_status_t status =
      device_->UsbFunctionInterfaceSetConfigured(/*configured=*/true, USB_SPEED_FULL);
  ASSERT_OK(status);
  auto callback = SetTestCallback();
  ASSERT_NOT_NULL(callback);
  ASSERT_TRUE(SendMagic());
  callback->WaitForSockets(1);
  ASSERT_TRUE(GetMagic());

  std::string_view test_data =
      "A basket of biscuits, a basket of mixed biscuits and a biscuit mixer.";
  ASSERT_TRUE(SendTx(reinterpret_cast<const uint8_t*>(test_data.data()), test_data.size()));
  ASSERT_TRUE(SocketReadExpect(&callback->sockets_[0],
                               reinterpret_cast<const uint8_t*>(test_data.data()),
                               test_data.size()));

  std::string_view test_data_b = "Aluminum, linoleum, magnesium, petroleum.";
  ASSERT_TRUE(SocketWriteAll(&callback->sockets_[0],
                             reinterpret_cast<const uint8_t*>(test_data_b.data()),
                             test_data_b.size()));
  ASSERT_TRUE(
      GetRxConcatExpect(reinterpret_cast<const uint8_t*>(test_data_b.data()), test_data_b.size()));

  ASSERT_TRUE(SendMagic());
  callback->WaitForSockets(2);
  ASSERT_TRUE(GetMagic());

  zx_signals_t pending;
  callback->sockets_[0].wait_one(ZX_SOCKET_PEER_CLOSED, zx::time::infinite(), &pending);
  ASSERT_NE(pending & ZX_SOCKET_PEER_CLOSED, 0);

  std::string_view test_data_c = "Around the rugged rocks the ragged rascals ran.";
  ASSERT_TRUE(SendTx(reinterpret_cast<const uint8_t*>(test_data_c.data()), test_data_c.size()));
  ASSERT_TRUE(SocketReadExpect(&callback->sockets_[1],
                               reinterpret_cast<const uint8_t*>(test_data_c.data()),
                               test_data_c.size()));

  std::string_view test_data_d = "A proper copper coffee pot.";
  ASSERT_TRUE(SocketWriteAll(&callback->sockets_[1],
                             reinterpret_cast<const uint8_t*>(test_data_d.data()),
                             test_data_d.size()));
  ASSERT_TRUE(
      GetRxConcatExpect(reinterpret_cast<const uint8_t*>(test_data_d.data()), test_data_d.size()));
}

TEST_F(OvernetUsbTest, ResetMoreData) {
  zx_status_t status =
      device_->UsbFunctionInterfaceSetConfigured(/*configured=*/true, USB_SPEED_FULL);
  ASSERT_OK(status);
  auto callback = SetTestCallback();
  ASSERT_NOT_NULL(callback);
  ASSERT_TRUE(SendMagic());
  callback->WaitForSockets(1);
  ASSERT_TRUE(GetMagic());

  std::string_view test_data_a =
      "A basket of biscuits, a basket of mixed biscuits and a biscuit mixer.";
  std::string_view test_data_b = "Aluminum, linoleum, magnesium, petroleum.";
  std::string_view test_data_c = "Around the rugged rocks the ragged rascals ran.";
  std::string_view test_data_d = "A proper copper coffee pot.";

  for (int i = 0; i < 50; i++) {
    ASSERT_TRUE(SendTx(reinterpret_cast<const uint8_t*>(test_data_a.data()), test_data_a.size()));
    ASSERT_TRUE(SocketReadExpect(&callback->sockets_[0],
                                 reinterpret_cast<const uint8_t*>(test_data_a.data()),
                                 test_data_a.size()));

    ASSERT_TRUE(SocketWriteAll(&callback->sockets_[0],
                               reinterpret_cast<const uint8_t*>(test_data_b.data()),
                               test_data_b.size()));
    ASSERT_TRUE(GetRxConcatExpect(reinterpret_cast<const uint8_t*>(test_data_b.data()),
                                  test_data_b.size()));

    ASSERT_TRUE(SendTx(reinterpret_cast<const uint8_t*>(test_data_c.data()), test_data_c.size()));
    ASSERT_TRUE(SocketReadExpect(&callback->sockets_[0],
                                 reinterpret_cast<const uint8_t*>(test_data_c.data()),
                                 test_data_c.size()));

    ASSERT_TRUE(SocketWriteAll(&callback->sockets_[0],
                               reinterpret_cast<const uint8_t*>(test_data_d.data()),
                               test_data_d.size()));
    ASSERT_TRUE(GetRxConcatExpect(reinterpret_cast<const uint8_t*>(test_data_d.data()),
                                  test_data_d.size()));
  }

  ASSERT_TRUE(SendMagic());
  callback->WaitForSockets(2);
  ASSERT_TRUE(GetMagic());

  zx_signals_t pending;
  callback->sockets_[0].wait_one(ZX_SOCKET_PEER_CLOSED, zx::time::infinite(), &pending);
  ASSERT_NE(pending & ZX_SOCKET_PEER_CLOSED, 0);

  for (int i = 0; i < 50; i++) {
    ASSERT_TRUE(SendTx(reinterpret_cast<const uint8_t*>(test_data_a.data()), test_data_a.size()));
    ASSERT_TRUE(SocketReadExpect(&callback->sockets_[1],
                                 reinterpret_cast<const uint8_t*>(test_data_a.data()),
                                 test_data_a.size()));

    ASSERT_TRUE(SocketWriteAll(&callback->sockets_[1],
                               reinterpret_cast<const uint8_t*>(test_data_b.data()),
                               test_data_b.size()));
    ASSERT_TRUE(GetRxConcatExpect(reinterpret_cast<const uint8_t*>(test_data_b.data()),
                                  test_data_b.size()));

    ASSERT_TRUE(SendTx(reinterpret_cast<const uint8_t*>(test_data_c.data()), test_data_c.size()));
    ASSERT_TRUE(SocketReadExpect(&callback->sockets_[1],
                                 reinterpret_cast<const uint8_t*>(test_data_c.data()),
                                 test_data_c.size()));

    ASSERT_TRUE(SocketWriteAll(&callback->sockets_[1],
                               reinterpret_cast<const uint8_t*>(test_data_d.data()),
                               test_data_d.size()));
    ASSERT_TRUE(GetRxConcatExpect(reinterpret_cast<const uint8_t*>(test_data_d.data()),
                                  test_data_d.size()));
  }
}

// NOLINTEND(readability-container-data-pointer)
// NOLINTEND(readability-convert-member-functions-to-static)
// NOLINTEND(misc-use-anonymous-namespace)
