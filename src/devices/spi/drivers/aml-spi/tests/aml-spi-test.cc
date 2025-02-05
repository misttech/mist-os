// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.scheduler/cpp/wire.h>

#include "src/devices/spi/drivers/aml-spi/tests/aml-spi-test-env.h"

namespace spi {

namespace {
bool IsBytesEqual(const uint8_t* expected, const uint8_t* actual, size_t len) {
  return memcmp(expected, actual, len) == 0;
}

zx_koid_t GetVmoKoid(const zx::vmo& vmo) {
  zx_info_handle_basic_t info = {};
  size_t actual = 0;
  size_t available = 0;
  zx_status_t status = vmo.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), &actual, &available);
  if (status != ZX_OK || actual < 1) {
    return ZX_KOID_INVALID;
  }
  return info.koid;
}

zx::result<fuchsia_scheduler::RoleName> GetSchedulerRoleName(
    fidl::WireClient<fuchsia_driver_compat::Device> client, fdf_testing::DriverRuntime& runtime) {
  zx::result<fuchsia_scheduler::RoleName> scheduler_role_name = zx::error(ZX_ERR_NOT_FOUND);
  client->GetMetadata().Then(
      [&](fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetMetadata>& result) {
        if (!result.ok()) {
          scheduler_role_name = zx::error(result.status());
          return;
        }
        if (result->is_error()) {
          scheduler_role_name = result->take_error();
          return;
        }

        for (auto& metadata : result->value()->metadata) {
          if (metadata.type != DEVICE_METADATA_SCHEDULER_ROLE_NAME) {
            continue;
          }

          size_t size = 0;
          zx_status_t status = metadata.data.get_prop_content_size(&size);
          if (status != ZX_OK) {
            continue;
          }

          std::vector<uint8_t> data(size);
          status = metadata.data.read(data.data(), 0, data.size());
          if (status != ZX_OK) {
            continue;
          }

          auto role_name = fidl::Unpersist<fuchsia_scheduler::RoleName>(std::move(data));
          if (role_name.is_error()) {
            continue;
          }

          scheduler_role_name = zx::ok(*std::move(role_name));
          break;
        }

        runtime.Quit();
      });
  runtime.Run();
  return scheduler_role_name;
}

}  // namespace

class AmlSpiTestConfig final {
 public:
  using DriverType = TestAmlSpiDriver;
  using EnvironmentType = BaseTestEnvironment;
};

class AmlSpiTest : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result<> result = driver_test().StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  fdf_testing::ForegroundDriverTest<AmlSpiTestConfig>& driver_test() { return driver_test_; }

 private:
  fdf_testing::ForegroundDriverTest<AmlSpiTestConfig> driver_test_;
};

TEST_F(AmlSpiTest, DdkLifecycle) {
  driver_test().RunInNodeContext([](fdf_testing::TestNode& node) {
    EXPECT_NE(node.children().find("aml-spi-0"), node.children().cend());
  });
}

TEST_F(AmlSpiTest, ChipSelectCount) {
  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(std::move(spiimpl_client.value()),
                                                             fdf::Dispatcher::GetCurrent()->get());
  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)->GetChipSelectCount().Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_EQ(result->count, 3u);
    driver_test().runtime().Quit();
  });
  driver_test().runtime().Run();
}

TEST_F(AmlSpiTest, Exchange) {
  uint8_t kTxData[] = {0x12, 0x12, 0x12, 0x12, 0x12, 0x12, 0x12};
  constexpr uint8_t kExpectedRxData[] = {0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab};

  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  driver_test().driver()->mmio()[AML_SPI_RXDATA].SetReadCallback(
      []() { return kExpectedRxData[0]; });

  uint64_t tx_data = 0;
  driver_test().driver()->mmio()[AML_SPI_TXDATA].SetWriteCallback(
      [&tx_data](uint64_t value) { tx_data = value; });

  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(kTxData, sizeof(kTxData)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok() && result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(kExpectedRxData));
        EXPECT_TRUE(
            IsBytesEqual(result->value()->rxdata.data(), kExpectedRxData, sizeof(kExpectedRxData)));
        driver_test().runtime().Quit();
      });
  driver_test().runtime().Run();

  EXPECT_EQ(tx_data, kTxData[0]);

  driver_test().RunInEnvironmentTypeContext([](BaseTestEnvironment& env) {
    EXPECT_FALSE(env.ControllerReset());
    EXPECT_EQ(env.cs_toggle_count(), 2u);
  });
}

TEST_F(AmlSpiTest, ExchangeCsManagedByClient) {
  uint8_t kTxData[] = {0x12, 0x12, 0x12, 0x12, 0x12, 0x12, 0x12};
  constexpr uint8_t kExpectedRxData[] = {0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab};

  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  driver_test().driver()->mmio()[AML_SPI_RXDATA].SetReadCallback(
      []() { return kExpectedRxData[0]; });

  uint64_t tx_data = 0;
  driver_test().driver()->mmio()[AML_SPI_TXDATA].SetWriteCallback(
      [&tx_data](uint64_t value) { tx_data = value; });

  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)
      ->ExchangeVector(2, fidl::VectorView<uint8_t>::FromExternal(kTxData, sizeof(kTxData)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok() && result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(kExpectedRxData));
        EXPECT_TRUE(
            IsBytesEqual(result->value()->rxdata.data(), kExpectedRxData, sizeof(kExpectedRxData)));
        driver_test().runtime().Quit();
      });
  driver_test().runtime().Run();

  EXPECT_EQ(tx_data, kTxData[0]);

  driver_test().RunInEnvironmentTypeContext([](BaseTestEnvironment& env) {
    EXPECT_FALSE(env.ControllerReset());

    // There should be no GPIO calls as the client manages CS for this device.
    EXPECT_EQ(env.cs_toggle_count(), 0u);
  });
}

TEST_F(AmlSpiTest, RegisterVmo) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  const zx_koid_t test_vmo_koid = GetVmoKoid(test_vmo);

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_error());
        });
  }

  spiimpl.buffer(arena)->UnregisterVmo(0, 1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(test_vmo_koid, GetVmoKoid(result->value()->vmo));
  });

  spiimpl.buffer(arena)->UnregisterVmo(0, 1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
    driver_test().runtime().Quit();
  });
  driver_test().runtime().Run();
}

TEST_F(AmlSpiTest, TransmitVmo) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  constexpr uint8_t kTxData[] = {0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5};

  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());
  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 256, PAGE_SIZE - 256}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  EXPECT_OK(test_vmo.write(kTxData, 512, sizeof(kTxData)));

  uint64_t tx_data = 0;
  driver_test().driver()->mmio()[AML_SPI_TXDATA].SetWriteCallback(
      [&tx_data](uint64_t value) { tx_data = value; });

  spiimpl.buffer(arena)->TransmitVmo(0, {1, 256, sizeof(kTxData)}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    driver_test().runtime().Quit();
  });
  driver_test().runtime().Run();

  EXPECT_EQ(tx_data, kTxData[0]);

  driver_test().RunInEnvironmentTypeContext([](BaseTestEnvironment& env) {
    EXPECT_FALSE(env.ControllerReset());
    EXPECT_EQ(env.cs_toggle_count(), 2u);
  });
}

TEST_F(AmlSpiTest, ReceiveVmo) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  constexpr uint8_t kExpectedRxData[] = {0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78};

  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 256, PAGE_SIZE - 256},
                      SharedVmoRight::kRead | SharedVmoRight::kWrite)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  driver_test().driver()->mmio()[AML_SPI_RXDATA].SetReadCallback(
      []() { return kExpectedRxData[0]; });

  spiimpl.buffer(arena)->ReceiveVmo(0, {1, 512, sizeof(kExpectedRxData)}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    driver_test().runtime().Quit();
  });
  driver_test().runtime().Run();

  uint8_t rx_buffer[sizeof(kExpectedRxData)];
  EXPECT_OK(test_vmo.read(rx_buffer, 768, sizeof(rx_buffer)));
  EXPECT_TRUE(IsBytesEqual(kExpectedRxData, rx_buffer, sizeof(rx_buffer)));

  driver_test().RunInEnvironmentTypeContext([](BaseTestEnvironment& env) {
    EXPECT_FALSE(env.ControllerReset());
    EXPECT_EQ(env.cs_toggle_count(), 2u);
  });
}

TEST_F(AmlSpiTest, ExchangeVmo) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  constexpr uint8_t kTxData[] = {0xef, 0xef, 0xef, 0xef, 0xef, 0xef, 0xef};
  constexpr uint8_t kExpectedRxData[] = {0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78};

  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 256, PAGE_SIZE - 256},
                      SharedVmoRight::kRead | SharedVmoRight::kWrite)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  driver_test().driver()->mmio()[AML_SPI_RXDATA].SetReadCallback(
      []() { return kExpectedRxData[0]; });

  uint64_t tx_data = 0;
  driver_test().driver()->mmio()[AML_SPI_TXDATA].SetWriteCallback(
      [&tx_data](uint64_t value) { tx_data = value; });

  EXPECT_OK(test_vmo.write(kTxData, 512, sizeof(kTxData)));

  spiimpl.buffer(arena)
      ->ExchangeVmo(0, {1, 256, sizeof(kTxData)}, {1, 512, sizeof(kExpectedRxData)})
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        driver_test().runtime().Quit();
      });
  driver_test().runtime().Run();

  uint8_t rx_buffer[sizeof(kExpectedRxData)];
  EXPECT_OK(test_vmo.read(rx_buffer, 768, sizeof(rx_buffer)));
  EXPECT_TRUE(IsBytesEqual(kExpectedRxData, rx_buffer, sizeof(rx_buffer)));

  EXPECT_EQ(tx_data, kTxData[0]);

  driver_test().RunInEnvironmentTypeContext([](BaseTestEnvironment& env) {
    EXPECT_FALSE(env.ControllerReset());
    EXPECT_EQ(env.cs_toggle_count(), 2u);
  });
}

TEST_F(AmlSpiTest, TransfersOutOfRange) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(1, 1, {std::move(vmo), PAGE_SIZE - 4, 4},
                      SharedVmoRight::kRead | SharedVmoRight::kWrite)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  spiimpl.buffer(arena)->ExchangeVmo(1, {1, 0, 2}, {1, 2, 2}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  spiimpl.buffer(arena)->ExchangeVmo(1, {1, 0, 2}, {1, 3, 2}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->ExchangeVmo(1, {1, 3, 2}, {1, 0, 2}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->ExchangeVmo(1, {1, 0, 3}, {1, 2, 3}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });

  spiimpl.buffer(arena)->TransmitVmo(1, {1, 0, 4}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  spiimpl.buffer(arena)->TransmitVmo(1, {1, 0, 5}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->TransmitVmo(1, {1, 3, 2}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->TransmitVmo(1, {1, 4, 1}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->TransmitVmo(1, {1, 5, 1}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });

  spiimpl.buffer(arena)->ReceiveVmo(1, {1, 0, 4}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });

  spiimpl.buffer(arena)->ReceiveVmo(1, {1, 3, 1}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });

  spiimpl.buffer(arena)->ReceiveVmo(1, {1, 3, 2}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->ReceiveVmo(1, {1, 4, 1}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->ReceiveVmo(1, {1, 5, 1}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
    driver_test().runtime().Quit();
  });
  driver_test().runtime().Run();

  driver_test().RunInEnvironmentTypeContext(
      [](BaseTestEnvironment& env) { EXPECT_EQ(env.cs_toggle_count(), 8u); });
}

TEST_F(AmlSpiTest, VmoBadRights) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 0, 256}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 2, {std::move(vmo), 0, 256},
                      SharedVmoRight::kRead | SharedVmoRight::kWrite)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  spiimpl.buffer(arena)->ExchangeVmo(0, {1, 0, 128}, {2, 128, 128}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  spiimpl.buffer(arena)->ExchangeVmo(0, {2, 0, 128}, {1, 128, 128}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_EQ(result->error_value(), ZX_ERR_ACCESS_DENIED);
  });
  spiimpl.buffer(arena)->ExchangeVmo(0, {1, 0, 128}, {1, 128, 128}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_EQ(result->error_value(), ZX_ERR_ACCESS_DENIED);
  });
  spiimpl.buffer(arena)->ReceiveVmo(0, {1, 0, 128}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_EQ(result->error_value(), ZX_ERR_ACCESS_DENIED);
    driver_test().runtime().Quit();
  });
  driver_test().runtime().Run();

  driver_test().RunInEnvironmentTypeContext(
      [](BaseTestEnvironment& env) { EXPECT_EQ(env.cs_toggle_count(), 2u); });
}

TEST_F(AmlSpiTest, Exchange64BitWords) {
  uint8_t kTxData[] = {
      0x3c, 0xa7, 0x5f, 0xc8, 0x4b, 0x0b, 0xdf, 0xef, 0xb9, 0xa0, 0xcb, 0xbd,
      0xd4, 0xcf, 0xa8, 0xbf, 0x85, 0xf2, 0x6a, 0xe3, 0xba, 0xf1, 0x49, 0x00,
  };
  constexpr uint8_t kExpectedRxData[] = {
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f,
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f,
  };

  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  // First (and only) word of kExpectedRxData with bytes swapped.
  driver_test().driver()->mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return 0xea2b'8f8f; });

  uint64_t tx_data = 0;
  driver_test().driver()->mmio()[AML_SPI_TXDATA].SetWriteCallback(
      [&tx_data](uint64_t value) { tx_data = value; });

  fdf::Arena arena('TEST');

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(kTxData, sizeof(kTxData)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok() && result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(kExpectedRxData));
        EXPECT_TRUE(
            IsBytesEqual(result->value()->rxdata.data(), kExpectedRxData, sizeof(kExpectedRxData)));
        driver_test().runtime().Quit();
      });
  driver_test().runtime().Run();

  // Last word of kTxData with bytes swapped.
  EXPECT_EQ(tx_data, 0xbaf1'4900);

  driver_test().RunInEnvironmentTypeContext([](BaseTestEnvironment& env) {
    EXPECT_FALSE(env.ControllerReset());
    EXPECT_EQ(env.cs_toggle_count(), 2u);
  });
}

TEST_F(AmlSpiTest, Exchange64Then8BitWords) {
  uint8_t kTxData[] = {
      0x3c, 0xa7, 0x5f, 0xc8, 0x4b, 0x0b, 0xdf, 0xef, 0xb9, 0xa0, 0xcb,
      0xbd, 0xd4, 0xcf, 0xa8, 0xbf, 0x85, 0xf2, 0x6a, 0xe3, 0xba,
  };
  constexpr uint8_t kExpectedRxData[] = {
      0x00, 0x00, 0x00, 0xea, 0x00, 0x00, 0x00, 0xea, 0x00, 0x00, 0x00,
      0xea, 0x00, 0x00, 0x00, 0xea, 0xea, 0xea, 0xea, 0xea, 0xea,
  };

  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  driver_test().driver()->mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return 0xea; });

  uint64_t tx_data = 0;
  driver_test().driver()->mmio()[AML_SPI_TXDATA].SetWriteCallback(
      [&tx_data](uint64_t value) { tx_data = value; });

  fdf::Arena arena('TEST');

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(kTxData, sizeof(kTxData)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok() && result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(kExpectedRxData));
        EXPECT_TRUE(
            IsBytesEqual(result->value()->rxdata.data(), kExpectedRxData, sizeof(kExpectedRxData)));
        driver_test().runtime().Quit();
      });
  driver_test().runtime().Run();

  EXPECT_EQ(tx_data, 0xbau);

  driver_test().RunInEnvironmentTypeContext([](BaseTestEnvironment& env) {
    EXPECT_FALSE(env.ControllerReset());
    EXPECT_EQ(env.cs_toggle_count(), 2u);
  });
}

TEST_F(AmlSpiTest, ExchangeResetsController) {
  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  fdf::Arena arena('TEST');

  uint8_t buf[17] = {};

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 17))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 17u);
        driver_test().RunInEnvironmentTypeContext(
            [](BaseTestEnvironment& env) { EXPECT_FALSE(env.ControllerReset()); });
        driver_test().runtime().Quit();
      });
  driver_test().runtime().Run();

  driver_test().RunInEnvironmentTypeContext(
      [](BaseTestEnvironment& env) { EXPECT_EQ(env.cs_toggle_count(), 2u); });

  // Controller should be reset because a 64-bit transfer was preceded by a transfer of an odd
  // number of bytes.
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 16))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 16u);
        driver_test().RunInEnvironmentTypeContext(
            [](BaseTestEnvironment& env) { EXPECT_TRUE(env.ControllerReset()); });
        driver_test().runtime().Quit();
      });
  driver_test().runtime().Run();

  driver_test().RunInEnvironmentTypeContext(
      [](BaseTestEnvironment& env) { EXPECT_EQ(env.cs_toggle_count(), 4u); });

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 3))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 3u);
        driver_test().RunInEnvironmentTypeContext(
            [](BaseTestEnvironment& env) { EXPECT_FALSE(env.ControllerReset()); });
        driver_test().runtime().Quit();
      });
  driver_test().runtime().Run();

  driver_test().RunInEnvironmentTypeContext(
      [](BaseTestEnvironment& env) { EXPECT_EQ(env.cs_toggle_count(), 6u); });

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 6))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 6u);
        driver_test().RunInEnvironmentTypeContext(
            [](BaseTestEnvironment& env) { EXPECT_FALSE(env.ControllerReset()); });
        driver_test().runtime().Quit();
      });
  driver_test().runtime().Run();

  driver_test().RunInEnvironmentTypeContext(
      [](BaseTestEnvironment& env) { EXPECT_EQ(env.cs_toggle_count(), 8u); });

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 8))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 8u);
        driver_test().RunInEnvironmentTypeContext(
            [](BaseTestEnvironment& env) { EXPECT_TRUE(env.ControllerReset()); });
        driver_test().runtime().Quit();
      });
  driver_test().runtime().Run();

  driver_test().RunInEnvironmentTypeContext(
      [](BaseTestEnvironment& env) { EXPECT_EQ(env.cs_toggle_count(), 10u); });
}

TEST_F(AmlSpiTest, ReleaseVmos) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });

    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 2, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  spiimpl.buffer(arena)->UnregisterVmo(0, 2).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });

  // Release VMO 1 and make sure that a subsequent call to unregister it fails.
  EXPECT_TRUE(spiimpl.buffer(arena)->ReleaseRegisteredVmos(0).ok());

  spiimpl.buffer(arena)->UnregisterVmo(0, 2).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });

  {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });

    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 2, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  // Release both VMOs and make sure that they can be registered again.
  EXPECT_TRUE(spiimpl.buffer(arena)->ReleaseRegisteredVmos(0).ok());

  {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });

    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 2, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
          driver_test().runtime().Quit();
        });
  }

  driver_test().runtime().Run();
}

TEST_F(AmlSpiTest, ReleaseVmosAfterClientsUnbind) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  auto spiimpl_client1 = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client1.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl1(*std::move(spiimpl_client1),
                                                              fdf::Dispatcher::GetCurrent()->get());

  fdf::Arena arena('TEST');

  // Register three VMOs through the first client.
  for (uint32_t i = 1; i <= 3; i++) {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl1.buffer(arena)
        ->RegisterVmo(0, i, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
          driver_test().runtime().Quit();
        });
    driver_test().runtime().Run();
    driver_test().runtime().ResetQuit();
  }

  auto spiimpl_client2 = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client2.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl2(*std::move(spiimpl_client2),
                                                              fdf::Dispatcher::GetCurrent()->get());

  // The second client should be able to see the registered VMOs.
  spiimpl2.buffer(arena)->UnregisterVmo(0, 1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    driver_test().runtime().Quit();
  });
  driver_test().runtime().Run();
  driver_test().runtime().ResetQuit();

  // Unbind the first client.
  EXPECT_TRUE(spiimpl1.UnbindMaybeGetEndpoint().is_ok());
  driver_test().runtime().RunUntilIdle();

  // The VMOs registered by the first client should remain.
  spiimpl2.buffer(arena)->UnregisterVmo(0, 2).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    driver_test().runtime().Quit();
  });
  driver_test().runtime().Run();
  driver_test().runtime().ResetQuit();

  // Unbind the second client, then connect a third client.
  EXPECT_TRUE(spiimpl2.UnbindMaybeGetEndpoint().is_ok());
  driver_test().runtime().RunUntilIdle();

  auto spiimpl_client3 = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client3.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl3(*std::move(spiimpl_client3),
                                                              fdf::Dispatcher::GetCurrent()->get());

  // All registered VMOs should have been released after the second client unbound.
  spiimpl3.buffer(arena)->UnregisterVmo(0, 3).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
    driver_test().runtime().Quit();
  });
  driver_test().runtime().Run();
}

class AmlSpiNoResetFragmentEnvironment : public BaseTestEnvironment {
 public:
  bool SetupResetRegister() override { return false; }
};

class AmlSpiNoResetFragmentConfig final {
 public:
  using DriverType = TestAmlSpiDriver;
  using EnvironmentType = AmlSpiNoResetFragmentEnvironment;
};

class AmlSpiNoResetFragmentTest : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result<> result = driver_test().StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  fdf_testing::ForegroundDriverTest<AmlSpiNoResetFragmentConfig>& driver_test() {
    return driver_test_;
  }

 private:
  fdf_testing::ForegroundDriverTest<AmlSpiNoResetFragmentConfig> driver_test_;
};

TEST_F(AmlSpiNoResetFragmentTest, ExchangeWithNoResetFragment) {
  auto spiimpl_client = driver_test().Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  fdf::Arena arena('TEST');

  uint8_t buf[17] = {};
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 17))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 17u);
        driver_test().RunInEnvironmentTypeContext(
            [](BaseTestEnvironment& env) { EXPECT_FALSE(env.ControllerReset()); });
      });

  // Controller should not be reset because no reset fragment was provided.
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 16))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 16u);
        driver_test().RunInEnvironmentTypeContext(
            [](BaseTestEnvironment& env) { EXPECT_FALSE(env.ControllerReset()); });
      });

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 3))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 3u);
        driver_test().RunInEnvironmentTypeContext(
            [](BaseTestEnvironment& env) { EXPECT_FALSE(env.ControllerReset()); });
      });

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 6))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 6u);
        driver_test().RunInEnvironmentTypeContext(
            [](BaseTestEnvironment& env) { EXPECT_FALSE(env.ControllerReset()); });
      });

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 8))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 8u);
        driver_test().RunInEnvironmentTypeContext(
            [](BaseTestEnvironment& env) { EXPECT_FALSE(env.ControllerReset()); });
        driver_test().runtime().Quit();
      });
  driver_test().runtime().Run();

  driver_test().RunInEnvironmentTypeContext(
      [](BaseTestEnvironment& env) { EXPECT_EQ(env.cs_toggle_count(), 10u); });
}

class AmlSpiNoIrqEnvironment : public BaseTestEnvironment {
  std::optional<zx::interrupt> CreateInterrupt() override { return std::nullopt; }
};

class AmlSpiNoIrqConfig final {
 public:
  using DriverType = TestAmlSpiDriver;
  using EnvironmentType = AmlSpiNoIrqEnvironment;
};

class AmlSpiNoIrqTest : public ::testing::Test {
 public:
  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  fdf_testing::ForegroundDriverTest<AmlSpiNoIrqConfig>& driver_test() { return driver_test_; }

 private:
  fdf_testing::ForegroundDriverTest<AmlSpiNoIrqConfig> driver_test_;
};

TEST_F(AmlSpiNoIrqTest, InterruptRequired) {
  // Bind should fail if no interrupt was provided.
  EXPECT_TRUE(driver_test().StartDriver().is_error());
}

TEST_F(AmlSpiTest, DefaultRoleMetadata) {
  constexpr char kExpectedRoleName[] = "fuchsia.devices.spi.drivers.aml-spi.transaction";

  zx::result compat_client_end = driver_test().Connect<fuchsia_driver_compat::Service::Device>();
  fidl::WireClient<fuchsia_driver_compat::Device> client(
      *std::move(compat_client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher());

  zx::result<fuchsia_scheduler::RoleName> metadata =
      GetSchedulerRoleName(std::move(client), driver_test().runtime());
  ASSERT_TRUE(metadata.is_ok());
  EXPECT_EQ(metadata->role(), kExpectedRoleName);
}

class AmlSpiForwardRoleMetadataEnvironment : public BaseTestEnvironment {
 public:
  void SetMetadata(compat::DeviceServer& compat) override {
    constexpr amlogic_spi::amlspi_config_t kSpiConfig = {
        .bus_id = 0,
        .cs_count = 3,
        .cs = {5, 3, amlogic_spi::amlspi_config_t::kCsClientManaged},
        .clock_divider_register_value = 0,
        .use_enhanced_clock_mode = false,
    };

    EXPECT_OK(compat.AddMetadata(DEVICE_METADATA_AMLSPI_CONFIG, &kSpiConfig, sizeof(kSpiConfig)));

    const fuchsia_scheduler::wire::RoleName role{kExpectedRoleName};

    fit::result result = fidl::Persist(role);
    ASSERT_TRUE(result.is_ok());

    EXPECT_OK(
        compat.AddMetadata(DEVICE_METADATA_SCHEDULER_ROLE_NAME, result->data(), result->size()));
  }

  static constexpr char kExpectedRoleName[] = "no.such.scheduler.role";
};

class AmlSpiForwardRoleMetadataConfig final {
 public:
  using DriverType = TestAmlSpiDriver;
  using EnvironmentType = AmlSpiForwardRoleMetadataEnvironment;
};

class AmlSpiForwardRoleMetadataTest : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result<> result = driver_test().StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  fdf_testing::ForegroundDriverTest<AmlSpiForwardRoleMetadataConfig>& driver_test() {
    return driver_test_;
  }

 private:
  fdf_testing::ForegroundDriverTest<AmlSpiForwardRoleMetadataConfig> driver_test_;
};

TEST_F(AmlSpiForwardRoleMetadataTest, Test) {
  zx::result compat_client_end = driver_test().Connect<fuchsia_driver_compat::Service::Device>();
  fidl::WireClient<fuchsia_driver_compat::Device> client(
      *std::move(compat_client_end), fdf::Dispatcher::GetCurrent()->async_dispatcher());

  zx::result<fuchsia_scheduler::RoleName> metadata =
      GetSchedulerRoleName(std::move(client), driver_test().runtime());
  ASSERT_TRUE(metadata.is_ok());
  EXPECT_EQ(metadata->role(), AmlSpiForwardRoleMetadataEnvironment::kExpectedRoleName);
}

}  // namespace spi
