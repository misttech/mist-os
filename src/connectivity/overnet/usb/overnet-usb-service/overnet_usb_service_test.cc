// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.overnet/cpp/fidl.h>
#include <fidl/fuchsia.hardware.overnet/cpp/wire.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <bind/fuchsia/test/cpp/bind.h>
#include <gtest/gtest.h>
namespace testing {

namespace {

namespace overnet = fuchsia_hardware_overnet;
constexpr uint8_t kOvernetMagic[] = "OVERNET USB\xff\x00\xff\x00\xff";
constexpr size_t kOvernetMagicSize = sizeof(kOvernetMagic) - 1;

}  // namespace

class FakeOvernetServer : public fidl::WireServer<overnet::Usb> {
 public:
  void SetCallback(SetCallbackRequestView request, SetCallbackCompleter::Sync& completer) override {
    current_callback_.emplace(std::move(request->callback));
    completer.Reply();
  }

  std::optional<fidl::ClientEnd<overnet::Callback>> current_callback_;
};

class OvernetServiceTestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    overnet::UsbService::InstanceHandler handler({
        .device =
            bindings_.CreateHandler(&server_, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                    fidl::kIgnoreBindingClosure),
    });
    auto result = to_driver_vfs.AddService<overnet::UsbService>(std::move(handler));
    EXPECT_EQ(ZX_OK, result.status_value());
    return zx::ok();
  }

  FakeOvernetServer server_;
  fidl::ServerBindingGroup<overnet::Usb> bindings_;
};

class TestConfig final {
 public:
  using DriverType = fdf_testing::EmptyDriverType;
  using EnvironmentType = OvernetServiceTestEnvironment;
};

class TestCallback : public fidl::WireServer<fuchsia_hardware_overnet::Callback> {
 public:
  TestCallback(const TestCallback&) = delete;
  TestCallback& operator=(const TestCallback&) = delete;
  TestCallback(size_t expected_calls, std::function<void(zx::socket)> callback)
      : expected_calls_(expected_calls), callback_(std::move(callback)) {}
  ~TestCallback() {
    printf("Destroying TestCallback %zu==%zu\n", expected_calls_, actual_calls_);
    EXPECT_EQ(expected_calls_, actual_calls_);
  }
  void NewLink(::fuchsia_hardware_overnet::wire::CallbackNewLinkRequest* request,
               NewLinkCompleter::Sync& completer) override {
    actual_calls_++;
    printf("calling callback %zu\n", actual_calls_);
    callback_(std::move(request->socket));
    completer.Reply();
  }

 private:
  size_t expected_calls_;
  size_t actual_calls_ = 0;
  std::function<void(zx::socket)> callback_;
};

class OvernetServiceTest : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result<> result = driver_test.StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());

    auto device = driver_test.Connect<fuchsia_hardware_overnet::Service::Device>();
    EXPECT_EQ(ZX_OK, device.status_value());
    client_.Bind(std::move(device.value()));
  }
  void TearDown() override {
    zx::result<> result = driver_test.StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  // Sets the callback for the service driver and returns an object that can be used to
  // get the sockets sent to that callback.
  std::unique_ptr<TestCallback> SetServiceCallback(
      size_t expected_calls, const std::function<void(zx::socket)>& callback) const {
    auto dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
    auto ret = std::make_unique<TestCallback>(expected_calls, callback);
    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_overnet::Callback>();
    if (!endpoints.is_ok()) {
      return nullptr;
    }
    fidl::BindServer(dispatcher, std::move(endpoints->server), ret.get());
    if (!client_->SetCallback(std::move(endpoints->client)).ok()) {
      return nullptr;
    }
    return ret;
  }

  // Waits for the service driver to register a callback with the USB driver layer
  void WaitForUsbCallback() {
    std::atomic_bool found_callback = false;
    while (!found_callback) {
      driver_test.RunInEnvironmentTypeContext([&found_callback](auto& env) {
        found_callback = env.server_.current_callback_.has_value();
      });
      driver_test.runtime().RunUntilIdle();
    }
  }

  zx::socket SendNewUsbSocket() {
    zx::socket usb_socket, other_end;
    EXPECT_EQ(ZX_OK, zx::socket::create(ZX_SOCKET_DATAGRAM, &usb_socket, &other_end));
    driver_test.RunInEnvironmentTypeContext([other_end = std::move(other_end)](auto& env) mutable {
      fidl::Call(*env.server_.current_callback_)->NewLink(std::move(other_end));
    });
    return usb_socket;
  }

  void SendPacket(zx::socket& socket, const void* buffer, size_t len) {
    size_t actual = 0;
    zx_status_t result = ZX_ERR_SHOULD_WAIT;
    while (result == ZX_ERR_SHOULD_WAIT && actual < len) {
      driver_test.runtime().RunUntilIdle();
      size_t written = 0;
      result = socket.write(0, buffer, len - actual, &written);
      actual += written;
    }
    ASSERT_EQ(ZX_OK, result);
    ASSERT_EQ(len, actual);
  }

  void ReadAndExpectPacket(zx::socket& socket, const void* expected_data, size_t expected_len) {
    size_t actual = 0;
    char buffer[4096] = {0};
    zx_status_t result = ZX_ERR_SHOULD_WAIT;
    while (result == ZX_ERR_SHOULD_WAIT && actual < expected_len) {
      driver_test.runtime().RunUntilIdle();
      size_t read = 0;
      result = socket.read(0, buffer, expected_len - actual, &read);
      actual += read;
    }
    ASSERT_EQ(ZX_OK, result);
    ASSERT_EQ(expected_len, actual);
    ASSERT_TRUE(0 == memcmp(expected_data, buffer, expected_len));
  }

  void SendMagicExchange(zx::socket& usb_socket) {
    SendPacket(usb_socket, kOvernetMagic, kOvernetMagicSize);
    ReadAndExpectPacket(usb_socket, kOvernetMagic, kOvernetMagicSize);
  }

  // Sends data out through the usb socket, verifies its receipt in the service socket, then
  // sends the data back and verifies its receipt on the usb socket side.
  void EchoData(zx::socket& usb_socket, zx::socket& service_socket, const void* buffer,
                size_t len) {
    SendPacket(usb_socket, buffer, len);
    ReadAndExpectPacket(service_socket, buffer, len);
    SendPacket(service_socket, buffer, len);
    ReadAndExpectPacket(usb_socket, buffer, len);
  }

  void ExpectSocketClosed(zx::socket& socket) {
    char buffer[1024];
    size_t read = 0;
    zx_status_t result = ZX_ERR_SHOULD_WAIT;
    while (result == ZX_ERR_SHOULD_WAIT) {
      driver_test.runtime().RunUntilIdle();
      result = socket.read(0, buffer, sizeof(buffer), &read);
    }
    ASSERT_EQ(ZX_ERR_PEER_CLOSED, result);
    ASSERT_EQ(read, 0ull);
  }

  fdf_testing::BackgroundDriverTest<TestConfig> driver_test;
  fidl::WireSyncClient<overnet::Device> client_;
};

TEST_F(OvernetServiceTest, DriverSetsCallback) {
  auto test_callback = SetServiceCallback(0, [](auto) {});
  WaitForUsbCallback();
}

TEST_F(OvernetServiceTest, DriverHandlesMagic) {
  std::optional<zx::socket> current_socket;
  auto test_callback = SetServiceCallback(
      1, [&current_socket](zx::socket socket) { current_socket.emplace(std::move(socket)); });
  WaitForUsbCallback();

  auto usb_socket = SendNewUsbSocket();
  SendMagicExchange(usb_socket);

  while (!current_socket) {
    driver_test.runtime().RunUntilIdle();
  }
}

TEST_F(OvernetServiceTest, DriverHandlesMagicAfterGarbage) {
  std::optional<zx::socket> current_socket;
  auto test_callback = SetServiceCallback(
      1, [&current_socket](zx::socket socket) { current_socket.emplace(std::move(socket)); });
  WaitForUsbCallback();

  auto usb_socket = SendNewUsbSocket();
  char garbage[] = "this is not a magic string";
  SendPacket(usb_socket, garbage, sizeof(garbage));
  SendMagicExchange(usb_socket);

  while (!current_socket) {
    driver_test.runtime().RunUntilIdle();
  }
}

TEST_F(OvernetServiceTest, DriverForwardsDataAfterMagic) {
  std::optional<zx::socket> current_socket;
  auto test_callback = SetServiceCallback(
      1, [&current_socket](zx::socket socket) { current_socket.emplace(std::move(socket)); });
  WaitForUsbCallback();

  auto usb_socket = SendNewUsbSocket();
  SendMagicExchange(usb_socket);

  while (!current_socket) {
    driver_test.runtime().RunUntilIdle();
  }

  char data[] = "this is some test data!";
  EchoData(usb_socket, *current_socket, data, sizeof(data));
}

TEST_F(OvernetServiceTest, DriverClosesSocketAfterUsbClose) {
  std::optional<zx::socket> current_socket;
  auto test_callback = SetServiceCallback(
      5, [&current_socket](zx::socket socket) { current_socket.emplace(std::move(socket)); });
  WaitForUsbCallback();

  for (auto i = 0; i < 5; i++) {
    auto usb_socket = SendNewUsbSocket();
    SendMagicExchange(usb_socket);

    while (!current_socket) {
      driver_test.runtime().RunUntilIdle();
    }

    char data[] = "this is some test data!";
    EchoData(usb_socket, *current_socket, data, sizeof(data));

    usb_socket.reset();
    ExpectSocketClosed(*current_socket);
    current_socket.reset();
  }
}

TEST_F(OvernetServiceTest, DriverResetsSocketWithMagic) {
  std::optional<zx::socket> current_socket;
  auto test_callback = SetServiceCallback(
      5, [&current_socket](zx::socket socket) { current_socket.emplace(std::move(socket)); });
  WaitForUsbCallback();
  auto usb_socket = SendNewUsbSocket();

  for (auto i = 0; i < 5; i++) {
    auto last_socket = std::move(current_socket);
    current_socket.reset();
    SendMagicExchange(usb_socket);
    if (last_socket) {
      ExpectSocketClosed(*last_socket);
    }

    while (!current_socket) {
      driver_test.runtime().RunUntilIdle();
    }
    char data[] = "this is some test data!";
    EchoData(usb_socket, *current_socket, data, sizeof(data));
  }
}

}  // namespace testing
