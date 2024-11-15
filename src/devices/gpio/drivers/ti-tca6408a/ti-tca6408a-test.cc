// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ti-tca6408a.h"

#include <fidl/fuchsia.hardware.pinimpl/cpp/driver/fidl.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/driver_lifecycle.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fake-i2c/fake-i2c.h>

#include <zxtest/zxtest.h>

namespace gpio {

class FakeTiTca6408aDevice : public fake_i2c::FakeI2c {
 public:
  uint8_t input_port() const { return input_port_; }
  void set_input_port(uint8_t input_port) { input_port_ = input_port; }
  uint8_t output_port() const { return output_port_; }
  uint8_t polarity_inversion() const { return polarity_inversion_; }
  uint8_t configuration() const { return configuration_; }

  fuchsia_hardware_i2c::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_i2c::Service::InstanceHandler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                          fidl::kIgnoreBindingClosure),
    });
  }

 protected:
  zx_status_t Transact(const uint8_t* write_buffer, size_t write_buffer_size, uint8_t* read_buffer,
                       size_t* read_buffer_size) override {
    if (write_buffer_size > 2) {
      return ZX_ERR_IO;
    }

    const uint8_t address = write_buffer[0];

    uint8_t* reg = nullptr;
    switch (address) {
      case 0:
        reg = &input_port_;
        break;
      case 1:
        reg = &output_port_;
        break;
      case 2:
        reg = &polarity_inversion_;
        break;
      case 3:
        reg = &configuration_;
        break;
      default:
        return ZX_ERR_IO;
    };

    if (write_buffer_size == 1) {
      *read_buffer = *reg;
      *read_buffer_size = 1;
    } else {
      *reg = write_buffer[1];
    }

    return ZX_OK;
  }

 private:
  uint8_t input_port_ = 0;
  uint8_t output_port_ = 0b1111'1111;
  uint8_t polarity_inversion_ = 0;
  uint8_t configuration_ = 0b1111'1111;
};

struct IncomingNamespace {
  fdf_testing::TestNode node_{std::string("root")};
  fdf_testing::internal::TestEnvironment env_{fdf::Dispatcher::GetCurrent()->get()};
  compat::DeviceServer device_server_;
  FakeTiTca6408aDevice fake_i2c_;
};

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
class TiTca6408aTest : public zxtest::Test {
 public:
  TiTca6408aTest()
      : env_dispatcher_(runtime_.StartBackgroundDispatcher()),
        driver_dispatcher_(runtime_.StartBackgroundDispatcher()),
        dut_(driver_dispatcher_->async_dispatcher(), std::in_place),
        incoming_(env_dispatcher_->async_dispatcher(), std::in_place) {}

  void SetUp() override {
    fuchsia_driver_framework::DriverStartArgs start_args;
    fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client;
    incoming_.SyncCall([&start_args, &outgoing_directory_client](IncomingNamespace* incoming) {
      auto start_args_result = incoming->node_.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());
      start_args = std::move(start_args_result->start_args);
      outgoing_directory_client = std::move(start_args_result->outgoing_directory_client);

      auto init_result =
          incoming->env_.Initialize(std::move(start_args_result->incoming_directory_server));
      ASSERT_TRUE(init_result.is_ok());

      incoming->device_server_.Initialize("pdev");

      // Serve fake_i2c_.
      auto result = incoming->env_.incoming_directory().AddService<fuchsia_hardware_i2c::Service>(
          std::move(incoming->fake_i2c_.GetInstanceHandler()), "i2c");
      ASSERT_TRUE(result.is_ok());
    });

    // Start dut_.
    auto result = runtime_.RunToCompletion(dut_.SyncCall(
        &fdf_testing::internal::DriverUnderTest<TiTca6408aDevice>::Start, std::move(start_args)));
    ASSERT_TRUE(result.is_ok());

    // Connect to PinImpl.
    auto svc_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();

    zx_status_t status = fdio_open_at(outgoing_directory_client.handle()->get(), "/svc",
                                      static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                                      svc_endpoints.server.TakeChannel().release());
    EXPECT_EQ(ZX_OK, status);

    auto connect_result =
        fdf::internal::DriverTransportConnect<fuchsia_hardware_pinimpl::Service::Device>(
            svc_endpoints.client, component::kDefaultInstance);
    ASSERT_TRUE(connect_result.is_ok());
    gpio_.Bind(std::move(connect_result.value()));
    ASSERT_TRUE(gpio_.is_valid());

    incoming_.SyncCall([](IncomingNamespace* incoming) {
      EXPECT_EQ(incoming->fake_i2c_.polarity_inversion(), 0);
    });
  }

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_;
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_;
  async_patterns::TestDispatcherBound<fdf_testing::internal::DriverUnderTest<TiTca6408aDevice>>
      dut_;

 protected:
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_;
  fdf::WireSyncClient<fuchsia_hardware_pinimpl::PinImpl> gpio_;
};

TEST_F(TiTca6408aTest, SetBufferMode) {
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1111'1111);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  fdf::Arena arena('TEST');
  {
    auto result =
        gpio_.buffer(arena)->SetBufferMode(0, fuchsia_hardware_gpio::BufferMode::kOutputLow);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1111'1110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1110);
  });

  {
    auto result = gpio_.buffer(arena)->SetBufferMode(0, fuchsia_hardware_gpio::BufferMode::kInput);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1111'1110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  {
    auto result =
        gpio_.buffer(arena)->SetBufferMode(5, fuchsia_hardware_gpio::BufferMode::kOutputLow);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  }
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1101'1110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1101'1111);
  });

  {
    auto result = gpio_.buffer(arena)->SetBufferMode(5, fuchsia_hardware_gpio::BufferMode::kInput);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1101'1110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  {
    auto result =
        gpio_.buffer(arena)->SetBufferMode(5, fuchsia_hardware_gpio::BufferMode::kOutputHigh);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1111'1110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1101'1111);
  });

  {
    auto result =
        gpio_.buffer(arena)->SetBufferMode(5, fuchsia_hardware_gpio::BufferMode::kOutputLow);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
}

TEST_F(TiTca6408aTest, Read) {
  incoming_.SyncCall([](IncomingNamespace* incoming) { incoming->fake_i2c_.set_input_port(0x55); });

  fdf::Arena arena('TEST');
  {
    auto result = gpio_.buffer(arena)->Read(0);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 1);
  };

  {
    auto result = gpio_.buffer(arena)->Read(3);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 0);
  };

  {
    auto result = gpio_.buffer(arena)->Read(4);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 1);
  };

  {
    auto result = gpio_.buffer(arena)->Read(7);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 0);
  };

  {
    auto result = gpio_.buffer(arena)->Read(5);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
}

TEST_F(TiTca6408aTest, InvalidArgs) {
  fdf::Arena arena('TEST');
  {
    auto result = gpio_.buffer(arena)->Read(7);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  {
    auto result = gpio_.buffer(arena)->Read(8);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
  };
  {
    auto result =
        gpio_.buffer(arena)->SetBufferMode(100, fuchsia_hardware_gpio::BufferMode::kOutputLow);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
  };
  {
    auto result =
        gpio_.buffer(arena)->SetBufferMode(101, fuchsia_hardware_gpio::BufferMode::kOutputHigh);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
  };
}

}  // namespace gpio
