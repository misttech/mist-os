// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "serial.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/zircon-internal/thread_annotations.h>

#include <memory>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

constexpr size_t kBufferLength = 16;

constexpr zx_signals_t kEventWrittenSignal = ZX_USER_SIGNAL_0;

// Fake for the SerialImpl protocol.
class FakeSerialImpl : public ddk::SerialImplProtocol<FakeSerialImpl> {
 public:
  FakeSerialImpl() : proto_({&serial_impl_protocol_ops_, this}) {
    zx::event::create(0, &write_event_);
  }

  // Getters.
  const serial_impl_protocol_t* proto() const { return &proto_; }
  bool enabled() const { return enabled_; }

  const serial_notify_t* callback() {
    fbl::AutoLock al(&cb_lock_);
    return &callback_;
  }

  char* read_buffer() { return read_buffer_; }
  const char* write_buffer() { return write_buffer_; }
  size_t write_buffer_length() const { return write_buffer_length_; }

  size_t total_written_bytes() const { return total_written_bytes_; }

  // Test utility methods.
  void set_state_and_notify(serial_state_t state) {
    fbl::AutoLock al(&cb_lock_);

    state_ = state;
    if (callback_.callback) {
      callback_.callback(callback_.ctx, state_);
    }
  }

  zx_status_t wait_for_write(zx::time deadline, zx_signals_t* pending) {
    return write_event_.wait_one(kEventWrittenSignal, deadline, pending);
  }

  zx_status_t SerialImplGetInfo(serial_port_info_t* info) { return ZX_OK; }

  zx_status_t SerialImplConfig(uint32_t baud_rate, uint32_t flags) { return ZX_OK; }

  zx_status_t SerialImplEnable(bool enable) {
    enabled_ = enable;
    return ZX_OK;
  }

  zx_status_t SerialImplRead(uint8_t* buf, size_t length, size_t* out_actual) {
    if (!(state_ & SERIAL_STATE_READABLE)) {
      *out_actual = 0;
      return ZX_ERR_SHOULD_WAIT;
    }

    char* buffer = reinterpret_cast<char*>(buf);
    size_t i;

    for (i = 0; i < length && i < kBufferLength && read_buffer_[i]; ++i) {
      buffer[i] = read_buffer_[i];
    }
    *out_actual = i;

    if (i == kBufferLength || !read_buffer_[i]) {
      // Simply reset the state, no advanced state machine.
      set_state_and_notify(0);
    }

    return ZX_OK;
  }

  zx_status_t SerialImplWrite(const uint8_t* buf, size_t length, size_t* out_actual) {
    if (!(state_ & SERIAL_STATE_WRITABLE)) {
      *out_actual = 0;
      return ZX_ERR_SHOULD_WAIT;
    }

    const char* buffer = reinterpret_cast<const char*>(buf);
    size_t i;

    for (i = 0; i < length && i < kBufferLength; ++i) {
      write_buffer_[i] = buffer[i];
    }
    *out_actual = i;

    // Signal that the write_buffer has been written to.
    if (i > 0) {
      write_buffer_length_ = i;
      total_written_bytes_ += i;
      write_event_.signal(0, kEventWrittenSignal);
    }

    return ZX_OK;
  }

  zx_status_t SerialImplSetNotifyCallback(const serial_notify_t* cb) {
    fbl::AutoLock al(&cb_lock_);
    callback_.callback = cb->callback;
    callback_.ctx = cb->ctx;
    return ZX_OK;
  }

 private:
  serial_impl_protocol_t proto_;
  bool enabled_;

  fbl::Mutex cb_lock_;
  serial_notify_t callback_ TA_GUARDED(cb_lock_) = {nullptr, nullptr};
  serial_state_t state_;

  char read_buffer_[kBufferLength];
  char write_buffer_[kBufferLength];
  size_t write_buffer_length_;
  size_t total_written_bytes_ = 0;

  zx::event write_event_;
};

class SerialTester {
 public:
  SerialTester() {
    fake_parent_->AddProtocol(ZX_PROTOCOL_SERIAL_IMPL, serial_impl_.proto()->ops,
                              serial_impl_.proto()->ctx);
  }
  FakeSerialImpl& serial_impl() { return serial_impl_; }
  zx_device_t* fake_parent() { return fake_parent_.get(); }

 private:
  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();
  FakeSerialImpl serial_impl_;
};

TEST(SerialTest, InitNoProtocolParent) {
  // SerialTester is intentionally not defined in this scope as it would
  // define the ZX_PROTOCOL_SERIAL_IMPL protocol.
  auto fake_parent = MockDevice::FakeRootParent();
  serial::SerialDevice device(fake_parent.get());
  ASSERT_EQ(ZX_ERR_NOT_SUPPORTED, device.Init());
}

TEST(SerialTest, Init) {
  SerialTester tester;
  serial::SerialDevice device(tester.fake_parent());
  ASSERT_EQ(ZX_OK, device.Init());
}

TEST(SerialTest, DdkLifetime) {
  SerialTester tester;
  serial::SerialDevice* device(new serial::SerialDevice(tester.fake_parent()));

  ASSERT_EQ(ZX_OK, device->Init());
  ASSERT_EQ(ZX_OK, device->Bind());
  device->DdkAsyncRemove();

  ASSERT_EQ(ZX_OK, mock_ddk::ReleaseFlaggedDevices(tester.fake_parent()));
}

TEST(SerialTest, DdkRelease) {
  SerialTester tester;
  serial::SerialDevice* device(new serial::SerialDevice(tester.fake_parent()));
  FakeSerialImpl& serial_impl = tester.serial_impl();

  ASSERT_EQ(ZX_OK, device->Init());

  // Manually set enabled to true.
  serial_impl.SerialImplEnable(true);
  EXPECT_TRUE(serial_impl.enabled());

  device->DdkRelease();

  EXPECT_FALSE(serial_impl.enabled());
  ASSERT_EQ(nullptr, serial_impl.callback()->callback);
}

// Provides control primitives for tests that issue IO requests to the device.
class SerialDeviceTest : public zxtest::Test {
 public:
  SerialDeviceTest();
  ~SerialDeviceTest() override;

  serial::SerialDevice* device() { return device_; }
  FakeSerialImpl& serial_impl() { return tester_.serial_impl(); }

  // DISALLOW_COPY_ASSIGN_AND_MOVE(SerialDeviceTest);

 private:
  SerialTester tester_;
  serial::SerialDevice* device_;
};

SerialDeviceTest::SerialDeviceTest() {
  device_ = new serial::SerialDevice(tester_.fake_parent());

  if (ZX_OK != device_->Init()) {
    delete device_;
    device_ = nullptr;
  }
}

SerialDeviceTest::~SerialDeviceTest() { device_->DdkRelease(); }

TEST_F(SerialDeviceTest, Read) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  auto [client_end, server] = fidl::Endpoints<fuchsia_hardware_serial::Device>::Create();
  fidl::BindServer(loop.dispatcher(), std::move(server), device());
  fidl::WireClient client(std::move(client_end), loop.dispatcher());

  constexpr std::string_view data = "test";

  // Test set up.
  *std::copy(data.begin(), data.end(), serial_impl().read_buffer()) = 0;
  serial_impl().set_state_and_notify(SERIAL_STATE_READABLE);

  // Test.
  client->Read().ThenExactlyOnce(
      [want = data](fidl::WireUnownedResult<fuchsia_hardware_serial::Device::Read>& result) {
        ASSERT_OK(result.status());
        const fit::result response = result.value();
        ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
        const cpp20::span data = response.value()->data.get();
        const std::string_view got{reinterpret_cast<const char*>(data.data()), data.size_bytes()};
        ASSERT_EQ(got, want);
      });
  ASSERT_OK(loop.RunUntilIdle());
}

TEST_F(SerialDeviceTest, Write) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  auto [client_end, server] = fidl::Endpoints<fuchsia_hardware_serial::Device>::Create();
  fidl::BindServer(loop.dispatcher(), std::move(server), device());
  fidl::WireClient client(std::move(client_end), loop.dispatcher());

  constexpr std::string_view data = "test";
  uint8_t payload[data.size()];
  std::copy(data.begin(), data.end(), payload);

  // Test set up.
  serial_impl().set_state_and_notify(SERIAL_STATE_WRITABLE);

  // Test.
  client->Write(fidl::VectorView<uint8_t>::FromExternal(payload))
      .ThenExactlyOnce(
          [this,
           want = data](fidl::WireUnownedResult<fuchsia_hardware_serial::Device::Write>& result) {
            ASSERT_OK(result.status());
            const fit::result response = result.value();
            ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
            const std::string_view got{serial_impl().write_buffer(),
                                       serial_impl().write_buffer_length()};
            ASSERT_EQ(got, want);
          });
  ASSERT_OK(loop.RunUntilIdle());
}

}  // namespace
