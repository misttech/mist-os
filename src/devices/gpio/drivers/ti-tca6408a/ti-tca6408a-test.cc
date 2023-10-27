// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ti-tca6408a.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/metadata.h>
#include <lib/fake-i2c/fake-i2c.h>

#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"

namespace gpio {

class FakeTiTca6408aDevice : public fake_i2c::FakeI2c {
 public:
  uint8_t input_port() const { return input_port_; }
  void set_input_port(uint8_t input_port) { input_port_ = input_port; }
  uint8_t output_port() const { return output_port_; }
  uint8_t polarity_inversion() const { return polarity_inversion_; }
  uint8_t configuration() const { return configuration_; }

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
  component::OutgoingDirectory outgoing_{async_get_default_dispatcher()};
  FakeTiTca6408aDevice fake_i2c_;
};

class TiTca6408aTest : public zxtest::Test {
 public:
  TiTca6408aTest()
      : ddk_(MockDevice::FakeRootParent()),
        dispatcher_(mock_ddk::GetDriverRuntime()->StartBackgroundDispatcher()),
        loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
        incoming_(loop_.dispatcher(), std::in_place) {
    ASSERT_OK(loop_.StartThread("incoming-ns-loop"));
  }

  void SetUp() override {
    constexpr uint32_t kPinIndexOffset = 100;
    ddk_->SetMetadata(DEVICE_METADATA_PRIVATE, &kPinIndexOffset, sizeof(kPinIndexOffset));

    auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ZX_ASSERT(endpoints.is_ok());
    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      auto service_result = incoming->outgoing_.AddService<fuchsia_hardware_i2c::Service>(
          fuchsia_hardware_i2c::Service::InstanceHandler(
              {.device = incoming->fake_i2c_.bind_handler(async_get_default_dispatcher())}));
      ZX_ASSERT(service_result.is_ok());

      ZX_ASSERT(incoming->outgoing_.Serve(std::move(endpoints->server)).is_ok());
    });

    ddk_->AddFidlService(fuchsia_hardware_i2c::Service::Name, std::move(endpoints->client), "i2c");

    // This TiTca6408a gets released by the MockDevice destructor.
    ASSERT_OK(TiTca6408a::Create(nullptr, ddk_.get()));

    MockDevice* device = ddk_->GetLatestChild();
    ASSERT_NOT_NULL(device);

    TiTca6408a* dut = device->GetDeviceContext<TiTca6408a>();
    ASSERT_NOT_NULL(dut);

    {
      auto endpoints = fdf::CreateEndpoints<fuchsia_hardware_gpioimpl::GpioImpl>();
      ASSERT_OK(endpoints.status_value());
      fdf::BindServer(dispatcher_->get(), std::move(endpoints->server), dut);
      gpio_.Bind(std::move(endpoints->client));
    }

    incoming_.SyncCall([](IncomingNamespace* incoming) {
      EXPECT_EQ(incoming->fake_i2c_.polarity_inversion(), 0);
    });
  }

 private:
  std::shared_ptr<MockDevice> ddk_;
  fdf::UnownedSynchronizedDispatcher dispatcher_;
  async::Loop loop_;

 protected:
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_;
  fdf::WireSyncClient<fuchsia_hardware_gpioimpl::GpioImpl> gpio_;
};

TEST_F(TiTca6408aTest, ConfigInOut) {
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1111'1111);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  fdf::Arena arena('TEST');
  {
    auto result = gpio_.buffer(arena)->ConfigOut(100, 0);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1111'1110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1110);
  });

  {
    auto result = gpio_.buffer(arena)->ConfigIn(100, fuchsia_hardware_gpio::GpioFlags::kNoPull);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1111'1110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  {
    auto result = gpio_.buffer(arena)->ConfigOut(105, 0);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  }
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1101'1110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1101'1111);
  });

  {
    auto result = gpio_.buffer(arena)->ConfigIn(105, fuchsia_hardware_gpio::GpioFlags::kNoPull);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1101'1110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  {
    auto result = gpio_.buffer(arena)->ConfigOut(105, 1);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1111'1110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1101'1111);
  });

  {
    auto result = gpio_.buffer(arena)->ConfigOut(107, 0);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
}

TEST_F(TiTca6408aTest, Read) {
  incoming_.SyncCall([](IncomingNamespace* incoming) { incoming->fake_i2c_.set_input_port(0x55); });

  fdf::Arena arena('TEST');
  {
    auto result = gpio_.buffer(arena)->Read(100);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 1);
  };

  {
    auto result = gpio_.buffer(arena)->Read(103);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 0);
  };

  {
    auto result = gpio_.buffer(arena)->Read(104);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 1);
  };

  {
    auto result = gpio_.buffer(arena)->Read(107);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 0);
  };

  {
    auto result = gpio_.buffer(arena)->Read(105);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
}

TEST_F(TiTca6408aTest, Write) {
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1111'1111);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  fdf::Arena arena('TEST');
  {
    auto result = gpio_.buffer(arena)->Write(100, 0);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1111'1110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  {
    auto result = gpio_.buffer(arena)->Write(101, 0);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1111'1100);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  {
    auto result = gpio_.buffer(arena)->Write(103, 0);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1111'0100);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  {
    auto result = gpio_.buffer(arena)->Write(104, 0);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1110'0100);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  {
    auto result = gpio_.buffer(arena)->Write(106, 0);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1010'0100);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  {
    auto result = gpio_.buffer(arena)->Write(101, 1);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1010'0110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });

  {
    auto result = gpio_.buffer(arena)->Write(104, 1);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  incoming_.SyncCall([](IncomingNamespace* incoming) {
    EXPECT_EQ(incoming->fake_i2c_.output_port(), 0b1011'0110);
    EXPECT_EQ(incoming->fake_i2c_.configuration(), 0b1111'1111);
  });
}

TEST_F(TiTca6408aTest, InvalidArgs) {
  fdf::Arena arena('TEST');
  {
    auto result = gpio_.buffer(arena)->ConfigIn(107, fuchsia_hardware_gpio::GpioFlags::kNoPull);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
  };
  {
    auto result = gpio_.buffer(arena)->ConfigIn(108, fuchsia_hardware_gpio::GpioFlags::kNoPull);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
  };
  {
    auto result = gpio_.buffer(arena)->ConfigIn(107, fuchsia_hardware_gpio::GpioFlags::kPullUp);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
  };
  {
    auto result = gpio_.buffer(arena)->ConfigIn(100, fuchsia_hardware_gpio::GpioFlags::kPullDown);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
  };
  {
    auto result = gpio_.buffer(arena)->ConfigOut(0, 0);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
  };
  {
    auto result = gpio_.buffer(arena)->ConfigOut(1, 1);
    ASSERT_TRUE(result.ok());
    ASSERT_FALSE(result->is_ok());
  };
}

}  // namespace gpio
