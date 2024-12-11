// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "registers.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/mock-mmio-reg/cpp/mock-mmio-reg.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <gtest/gtest.h>

#include "src/devices/lib/fidl-metadata/registers.h"

namespace registers {

namespace {

constexpr uint32_t kRegCount = 3;
constexpr size_t kRegSize = 0x00000100;

}  // namespace

class TestRegistersDevice : public RegistersDevice {
 public:
  TestRegistersDevice(fdf::DriverStartArgs start_args,
                      fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : RegistersDevice(std::move(start_args), std::move(driver_dispatcher)) {}
  ~TestRegistersDevice() override {
    for (const auto& i : mock_mmio_) {
      i->VerifyAll();
    }
  }

  static DriverRegistration GetDriverRegistration() {
    return FUCHSIA_DRIVER_REGISTRATION_V1(
        fdf_internal::DriverServer<TestRegistersDevice>::initialize,
        fdf_internal::DriverServer<TestRegistersDevice>::destroy);
  }

  zx::result<> MapMmio(fuchsia_hardware_registers::wire::Mask::Tag& tag) override {
    std::map<uint32_t, std::shared_ptr<MmioInfo>> mmios;
    for (uint32_t i = 0; i < kRegCount; i++) {
      mock_mmio_[i] = std::make_unique<mock_mmio::MockMmioRegRegion>(
          SWITCH_BY_TAG(tag, GetSize), kRegSize / SWITCH_BY_TAG(tag, GetSize));

      zx::result<MmioInfo> mmio_info =
          SWITCH_BY_TAG(tag, MmioInfo::Create, mock_mmio_[i]->GetMmioBuffer());
      EXPECT_TRUE(mmio_info.is_ok());
      mmios_.emplace(i, std::make_shared<MmioInfo>(std::move(*mmio_info)));
    }

    return zx::ok();
  }

  std::array<std::unique_ptr<mock_mmio::MockMmioRegRegion>, kRegCount> mock_mmio_;
};

class RegistersDeviceTestEnvironment : fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    device_server_.Initialize(component::kDefaultInstance);
    device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &to_driver_vfs);

    return zx::ok();
  }

  template <typename T>
  void Init(std::vector<fidl_metadata::registers::Register<T>>& kRegisters) {
    auto metadata = fidl_metadata::registers::RegistersMetadataToFidl(kRegisters);
    ASSERT_TRUE(metadata.is_ok());
    auto status =
        device_server_.AddMetadata(DEVICE_METADATA_REGISTERS, metadata->data(), metadata->size());
    ASSERT_EQ(ZX_OK, status);
  }

 private:
  compat::DeviceServer device_server_;
};

class RegistersDeviceTestConfig final {
 public:
  using DriverType = TestRegistersDevice;
  using EnvironmentType = RegistersDeviceTestEnvironment;
};

class RegistersDeviceTest : public ::testing::Test {
 public:
  template <typename T>
  void Init(std::vector<fidl_metadata::registers::Register<T>>&& kRegisters) {
    driver_test().RunInEnvironmentTypeContext(
        [&kRegisters](RegistersDeviceTestEnvironment& env) { env.Init(kRegisters); });
    EXPECT_TRUE(driver_test().StartDriver().is_ok());
  }

  fidl::ClientEnd<fuchsia_hardware_registers::Device> GetClient(std::string&& name) {
    auto result = driver_test().Connect<fuchsia_hardware_registers::Service::Device>(name);
    EXPECT_EQ(ZX_OK, result.status_value());
    return std::move(result.value());
  }

  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  fdf_testing::BackgroundDriverTest<RegistersDeviceTestConfig>& driver_test() {
    return driver_test_;
  }

 private:
  fdf_testing::BackgroundDriverTest<RegistersDeviceTestConfig> driver_test_;
};

TEST_F(RegistersDeviceTest, Read32Test) {
  Init(std::vector<fidl_metadata::registers::Register<uint32_t>>{
      {
          .name = "test0",
          .mmio_id = 0,
          .masks =
              {
                  {
                      .value = 0xFFFFFFFF,
                      .mmio_offset = 0,
                  },
              },
      },
      {
          .name = "test1",
          .mmio_id = 2,
          .masks =
              {
                  {
                      .value = 0xFFFFFFFF,
                      .mmio_offset = 0,
                      .count = 2,
                  },
                  {
                      .value = 0xFFFF0000,
                      .mmio_offset = 0x8,
                  },
              },
      },
  });

  auto test0 = fidl::WireSyncClient<fuchsia_hardware_registers::Device>(GetClient("test0"));
  auto test1 = fidl::WireSyncClient<fuchsia_hardware_registers::Device>(GetClient("test1"));

  // Invalid Call
  {
    auto result = test0->ReadRegister8(/* offset: */ 0x0, /* mask: */ 0xFF);
    EXPECT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Address not aligned
  {
    auto result = test0->ReadRegister32(/* offset: */ 0x1, /* mask: */ 0xFFFFFFFF);
    EXPECT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Address out of range
  {
    auto result = test1->ReadRegister32(/* offset: */ 0xC, /* mask: */ 0xFFFFFFFF);
    EXPECT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Invalid mask
  {
    auto result = test1->ReadRegister32(/* offset: */ 0x8, /* mask: */ 0xFFFFFFFF);
    EXPECT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Successful
  driver_test().RunInDriverContext(
      [](TestRegistersDevice& driver) { (*(driver.mock_mmio_[0]))[0x0].ExpectRead(0x12341234); });
  {
    auto result = test0->ReadRegister32(/* offset: */ 0x0, /* mask: */ 0xFFFFFFFF);
    EXPECT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 0x12341234U);
  };

  driver_test().RunInDriverContext(
      [](TestRegistersDevice& driver) { (*(driver.mock_mmio_[2]))[0x4].ExpectRead(0x12341234); });
  {
    auto result = test1->ReadRegister32(/* offset: */ 0x4, /* mask: */ 0xFFFF0000);
    EXPECT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 0x12340000U);
  };

  driver_test().RunInDriverContext(
      [](TestRegistersDevice& driver) { (*(driver.mock_mmio_[2]))[0x8].ExpectRead(0x12341234); });
  {
    auto result = test1->ReadRegister32(/* offset: */ 0x8, /* mask: */ 0xFFFF0000);
    EXPECT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 0x12340000U);
  };
}

TEST_F(RegistersDeviceTest, Write32Test) {
  Init(std::vector<fidl_metadata::registers::Register<uint32_t>>{
      {
          .name = "test0",
          .mmio_id = 0,
          .masks =
              {
                  {
                      .value = 0xFFFFFFFF,
                      .mmio_offset = 0,
                  },
              },
      },
      {
          .name = "test1",
          .mmio_id = 1,
          .masks =
              {
                  {
                      .value = 0xFFFFFFFF,
                      .mmio_offset = 0,
                      .count = 2,
                  },
                  {
                      .value = 0xFFFF0000,
                      .mmio_offset = 0x8,
                  },
              },
      },
  });

  auto test0 = fidl::WireSyncClient<fuchsia_hardware_registers::Device>(GetClient("test0"));
  auto test1 = fidl::WireSyncClient<fuchsia_hardware_registers::Device>(GetClient("test1"));

  // Invalid Call
  {
    auto result = test0->WriteRegister8(/* offset: */ 0x0, /* mask: */ 0xFF, /* value:  */ 0x12);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Address not aligned
  {
    auto result = test0->WriteRegister32(
        /* offset: */ 0x1, /* mask: */ 0xFFFFFFFF, /* value: */ 0x43214321);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Address out of range
  {
    auto result = test1->WriteRegister32(
        /* offset: */ 0xC, /* mask: */ 0xFFFFFFFF, /* value: */ 0x43214321);
    EXPECT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Invalid mask
  {
    auto result = test1->WriteRegister32(
        /* offset: */ 0x8, /* mask: */ 0xFFFFFFFF, /* value: */ 0x43214321);
    EXPECT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Successful
  driver_test().RunInDriverContext([](TestRegistersDevice& driver) {
    (*(driver.mock_mmio_[0]))[0x0].ExpectRead(0x00000000).ExpectWrite(0x43214321);
  });
  {
    auto result = test0->WriteRegister32(
        /* offset: */ 0x0, /* mask: */ 0xFFFFFFFF, /* value: */
        0x43214321);
    EXPECT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  };

  driver_test().RunInDriverContext([](TestRegistersDevice& driver) {
    (*(driver.mock_mmio_[1]))[0x4].ExpectRead(0x00000000).ExpectWrite(0x43210000);
  });
  {
    auto result = test1->WriteRegister32(
        /* offset: */ 0x4, /* mask: */ 0xFFFF0000, /* value: */
        0x43214321);
    EXPECT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  };

  driver_test().RunInDriverContext([](TestRegistersDevice& driver) {
    (*(driver.mock_mmio_[1]))[0x8].ExpectRead(0x00000000).ExpectWrite(0x43210000);
  });
  {
    auto result = test1->WriteRegister32(
        /* offset: */ 0x8, /* mask: */ 0xFFFF0000, /* value: */
        0x43214321);
    EXPECT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  };
}

TEST_F(RegistersDeviceTest, Read64Test) {
  Init(std::vector<fidl_metadata::registers::Register<uint64_t>>{
      {
          .name = "test0",
          .mmio_id = 0,
          .masks =
              {
                  {
                      .value = 0xFFFFFFFFFFFFFFFF,
                      .mmio_offset = 0,
                  },
              },
      },
      {
          .name = "test1",
          .mmio_id = 2,
          .masks =
              {
                  {
                      .value = 0xFFFFFFFFFFFFFFFF,
                      .mmio_offset = 0,
                  },
                  {
                      .value = 0x00000000FFFFFFFF,
                      .mmio_offset = 0x8,
                  },
                  {
                      .value = 0xFFFFFFFF00000000,
                      .mmio_offset = 0x10,
                  },
              },
      },
  });

  auto test0 = fidl::WireSyncClient<fuchsia_hardware_registers::Device>(GetClient("test0"));
  auto test1 = fidl::WireSyncClient<fuchsia_hardware_registers::Device>(GetClient("test1"));

  // Invalid Call
  {
    auto result = test0->ReadRegister8(/* offset: */ 0x0, /* mask: */ 0xFF);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Address not aligned
  {
    auto result = test0->ReadRegister64(/* offset: */ 0x1, /* mask: */
                                        0xFFFFFFFFFFFFFFFF);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Address out of range
  {
    auto result = test1->ReadRegister64(/* offset: */ 0x20, /* mask: */ 0xFFFFFFFFFFFFFFFF);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Invalid mask
  {
    auto result = test1->ReadRegister64(/* offset: */ 0x8, /* mask: */ 0xFFFFFFFFFFFFFFFF);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Successful
  driver_test().RunInDriverContext([](TestRegistersDevice& driver) {
    (*(driver.mock_mmio_[0]))[0x0].ExpectRead(0x1234123412341234);
  });
  {
    auto result = test0->ReadRegister64(/* offset: */ 0x0, /* mask: */ 0xFFFFFFFFFFFFFFFF);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 0x1234123412341234UL);
  };

  driver_test().RunInDriverContext([](TestRegistersDevice& driver) {
    (*(driver.mock_mmio_[2]))[0x8].ExpectRead(0x1234123412341234);
  });
  {
    auto result = test1->ReadRegister64(/* offset: */ 0x8, /* mask: */ 0x00000000FFFF0000);
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 0x0000000012340000UL);
  };
}

TEST_F(RegistersDeviceTest, Write64Test) {
  Init(std::vector<fidl_metadata::registers::Register<uint64_t>>{
      {
          .name = "test0",
          .mmio_id = 0,
          .masks =
              {
                  {
                      .value = 0xFFFFFFFFFFFFFFFF,
                      .mmio_offset = 0,
                  },
              },
      },
      {
          .name = "test1",
          .mmio_id = 1,
          .masks =
              {
                  {
                      .value = 0xFFFFFFFFFFFFFFFF,
                      .mmio_offset = 0,
                  },
                  {
                      .value = 0x00000000FFFFFFFF,
                      .mmio_offset = 0x8,
                  },
                  {
                      .value = 0xFFFFFFFF00000000,
                      .mmio_offset = 0x10,
                  },
              },
      },
  });

  auto test0 = fidl::WireSyncClient<fuchsia_hardware_registers::Device>(GetClient("test0"));
  auto test1 = fidl::WireSyncClient<fuchsia_hardware_registers::Device>(GetClient("test1"));

  // Invalid Call
  {
    auto result = test0->WriteRegister8(/* offset: */ 0x0, /* mask: */ 0xFF, /* value:  */ 0x12);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Address not aligned
  {
    auto result = test0->WriteRegister64(
        /* offset: */ 0x1, /* mask: */ 0xFFFFFFFFFFFFFFFF, /* value: */
        0x4321432143214321);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Address out of range
  {
    auto result = test1->WriteRegister64(
        /* offset: */ 0x20, /* mask: */ 0xFFFFFFFFFFFFFFFF, /* value: */
        0x4321432143214321);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Invalid mask
  {
    auto result = test1->WriteRegister64(/* offset: */ 0x8,
                                         /* mask: */ 0xFFFFFFFFFFFFFFFF,
                                         /* value: */ 0x4321432143214321);
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
  };

  // Successful
  driver_test().RunInDriverContext([](TestRegistersDevice& driver) {
    (*(driver.mock_mmio_[0]))[0x0].ExpectRead(0x0000000000000000).ExpectWrite(0x4321432143214321);
  });
  {
    auto result = test0->WriteRegister64(
        /* offset: */ 0x0, /* mask: */ 0xFFFFFFFFFFFFFFFF, /* value: */
        0x4321432143214321);
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  };

  driver_test().RunInDriverContext([](TestRegistersDevice& driver) {
    (*(driver.mock_mmio_[1]))[0x8].ExpectRead(0x0000000000000000).ExpectWrite(0x0000000043210000);
  });
  {
    auto result = test1->WriteRegister64(
        /* offset: */ 0x8, /* mask: */ 0x00000000FFFF0000, /* value: */
        0x0000000043210000);
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  };
}

}  // namespace registers
