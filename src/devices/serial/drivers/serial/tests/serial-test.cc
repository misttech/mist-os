// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "serial.h"

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/fit/function.h>

#include <memory>

#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

constexpr size_t kBufferLength = 16;

constexpr zx_signals_t kEventWrittenSignal = ZX_USER_SIGNAL_0;

// Fake for the SerialImpl protocol.
class FakeSerialImpl : public fdf::WireServer<fuchsia_hardware_serialimpl::Device> {
 public:
  FakeSerialImpl() { zx::event::create(0, &write_event_); }

  // Getters.
  bool enabled() const { return enabled_; }
  void set_enabled(bool enabled) { enabled_ = enabled; }

  uint8_t* read_buffer() { return read_buffer_; }
  const uint8_t* write_buffer() { return write_buffer_; }
  size_t write_buffer_length() const { return write_buffer_length_; }

  size_t total_written_bytes() const { return total_written_bytes_; }

  // Test utility methods.
  zx_status_t wait_for_write(zx::time deadline, zx_signals_t* pending) {
    return write_event_.wait_one(kEventWrittenSignal, deadline, pending);
  }

  void Bind(fdf::ServerEnd<fuchsia_hardware_serialimpl::Device> server) {
    bindings_.AddBinding(fdf::Dispatcher::GetCurrent()->get(), std::move(server), this,
                         fidl::kIgnoreBindingClosure);
  }

  void GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess({fuchsia_hardware_serial::Class::kGeneric});
  }

  void Config(fuchsia_hardware_serialimpl::wire::DeviceConfigRequest* request, fdf::Arena& arena,
              ConfigCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess();
  }

  void Enable(fuchsia_hardware_serialimpl::wire::DeviceEnableRequest* request, fdf::Arena& arena,
              EnableCompleter::Sync& completer) override {
    enabled_ = request->enable;
    completer.buffer(arena).ReplySuccess();
  }

  void Read(fdf::Arena& arena, ReadCompleter::Sync& completer) override {
    fidl::VectorView<uint8_t> buffer(arena, kBufferLength);
    size_t i;

    for (i = 0; i < kBufferLength && read_buffer_[i]; ++i) {
      buffer[i] = read_buffer_[i];
    }
    buffer.set_count(i);

    completer.buffer(arena).ReplySuccess(buffer);
  }

  void Write(fuchsia_hardware_serialimpl::wire::DeviceWriteRequest* request, fdf::Arena& arena,
             WriteCompleter::Sync& completer) override {
    size_t i;

    for (i = 0; i < request->data.count() && i < kBufferLength; ++i) {
      write_buffer_[i] = request->data[i];
    }

    // Signal that the write_buffer has been written to.
    if (i > 0) {
      write_buffer_length_ = i;
      total_written_bytes_ += i;
      write_event_.signal(0, kEventWrittenSignal);
    }

    completer.buffer(arena).ReplySuccess();
  }

  void CancelAll(fdf::Arena& arena, CancelAllCompleter::Sync& completer) override {
    completer.buffer(arena).Reply();
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_serialimpl::Device> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    FAIL();
  }

 private:
  bool enabled_;

  uint8_t read_buffer_[kBufferLength];
  uint8_t write_buffer_[kBufferLength];
  size_t write_buffer_length_;
  size_t total_written_bytes_ = 0;

  zx::event write_event_;

  fdf::ServerBindingGroup<fuchsia_hardware_serialimpl::Device> bindings_;
};

class SerialTester {
 public:
  SerialTester()
      : runtime_(mock_ddk::GetDriverRuntime()),
        incoming_dispatcher_(runtime_->StartBackgroundDispatcher()),
        serial_impl_(incoming_dispatcher_->async_dispatcher(), std::in_place) {}

  auto& serial_impl() { return serial_impl_; }
  zx_device_t* fake_parent() { return fake_parent_.get(); }
  auto& runtime() { return *runtime_; }

  fdf::WireClient<fuchsia_hardware_serialimpl::Device> CreateSerialImplClient() {
    auto [client, server] = fdf::Endpoints<fuchsia_hardware_serialimpl::Device>::Create();
    serial_impl_.SyncCall(&FakeSerialImpl::Bind, std::move(server));
    return fdf::WireClient(std::move(client), fdf::Dispatcher::GetCurrent()->get());
  }

 private:
  std::shared_ptr<fdf_testing::DriverRuntime> runtime_;
  fdf::UnownedSynchronizedDispatcher incoming_dispatcher_;
  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();
  async_patterns::TestDispatcherBound<FakeSerialImpl> serial_impl_;
};

TEST(SerialTest, InitNoProtocolParent) {
  // SerialTester is intentionally not defined in this scope as it would
  // define the ZX_PROTOCOL_SERIAL_IMPL protocol.
  auto fake_parent = MockDevice::FakeRootParent();
  serial::SerialDevice device(fake_parent.get(), fake_parent.get());
  ASSERT_EQ(ZX_ERR_NOT_SUPPORTED, device.Init());
}

TEST(SerialTest, Init) {
  SerialTester tester;
  serial::SerialDevice device(tester.fake_parent(), tester.CreateSerialImplClient());
  ASSERT_EQ(ZX_OK, device.Init());
}

TEST(SerialTest, DdkLifetime) {
  SerialTester tester;
  serial::SerialDevice* device(
      new serial::SerialDevice(tester.fake_parent(), tester.CreateSerialImplClient()));

  ASSERT_EQ(ZX_OK, device->Init());
  ASSERT_EQ(ZX_OK, device->Bind());
  device->DdkAsyncRemove();

  ASSERT_EQ(ZX_OK, mock_ddk::ReleaseFlaggedDevices(tester.fake_parent()));
}

TEST(SerialTest, DdkRelease) {
  SerialTester tester;
  serial::SerialDevice* device(
      new serial::SerialDevice(tester.fake_parent(), tester.CreateSerialImplClient()));
  ASSERT_EQ(ZX_OK, device->Init());

  // Manually set enabled to true.
  tester.serial_impl().SyncCall(&FakeSerialImpl::set_enabled, true);

  device->DdkRelease();

  EXPECT_FALSE(tester.serial_impl().SyncCall(&FakeSerialImpl::enabled));
}

// Provides control primitives for tests that issue IO requests to the device.
class SerialDeviceTest : public zxtest::Test {
 public:
  SerialDeviceTest();
  ~SerialDeviceTest() override;

  serial::SerialDevice* device() { return device_; }
  auto& serial_impl() { return tester_.serial_impl(); }
  auto& runtime() { return tester_.runtime(); }

  // DISALLOW_COPY_ASSIGN_AND_MOVE(SerialDeviceTest);

 private:
  SerialTester tester_;
  serial::SerialDevice* device_;
};

SerialDeviceTest::SerialDeviceTest() {
  device_ = new serial::SerialDevice(tester_.fake_parent(), tester_.CreateSerialImplClient());

  if (ZX_OK != device_->Init()) {
    delete device_;
    device_ = nullptr;
  }
}

SerialDeviceTest::~SerialDeviceTest() { device_->DdkRelease(); }

TEST_F(SerialDeviceTest, Read) {
  auto [client_end, server] = fidl::Endpoints<fuchsia_hardware_serial::Device>::Create();
  fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server), device());
  fidl::WireClient client(std::move(client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher());

  constexpr std::string_view data = "test";

  // Test set up.
  serial_impl().SyncCall([&data](FakeSerialImpl* serial_impl) {
    *std::copy(data.begin(), data.end(), serial_impl->read_buffer()) = 0;
  });

  // Test.
  client->Read().ThenExactlyOnce(
      [this, want = data](fidl::WireUnownedResult<fuchsia_hardware_serial::Device::Read>& result) {
        ASSERT_OK(result.status());
        const fit::result response = result.value();
        ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
        const cpp20::span data = response.value()->data.get();
        const std::string_view got{reinterpret_cast<const char*>(data.data()), data.size_bytes()};
        ASSERT_EQ(got, want);
        runtime().Quit();
      });
  runtime().Run();
}

TEST_F(SerialDeviceTest, Write) {
  auto [client_end, server] = fidl::Endpoints<fuchsia_hardware_serial::Device>::Create();
  fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server), device());
  fidl::WireClient client(std::move(client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher());

  constexpr std::string_view data = "test";
  uint8_t payload[data.size()];
  std::copy(data.begin(), data.end(), payload);

  // Test.
  client->Write(fidl::VectorView<uint8_t>::FromExternal(payload))
      .ThenExactlyOnce(
          [this,
           want = data](fidl::WireUnownedResult<fuchsia_hardware_serial::Device::Write>& result) {
            ASSERT_OK(result.status());
            const fit::result response = result.value();
            ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
            serial_impl().SyncCall([&want](FakeSerialImpl* serial_impl) {
              ASSERT_EQ(serial_impl->write_buffer_length(), want.size());
              ASSERT_BYTES_EQ(serial_impl->write_buffer(), want.data(), want.size());
            });
            runtime().Quit();
          });
  runtime().Run();
}

}  // namespace
