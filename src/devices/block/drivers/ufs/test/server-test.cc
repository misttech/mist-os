// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.ufs/cpp/common_types.h>
#include <fidl/fuchsia.hardware.ufs/cpp/markers.h>
#include <fidl/fuchsia.hardware.ufs/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.ufs/cpp/wire_types.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/vector_view.h>
#include <lib/fidl/cpp/wire/wire_types.h>
#include <zircon/errors.h>

#include <cstdint>

#include <gtest/gtest.h>

#include "src/devices/block/drivers/ufs/device_manager.h"
#include "src/devices/block/drivers/ufs/upiu/attributes.h"
#include "src/devices/block/drivers/ufs/upiu/descriptors.h"
#include "src/devices/block/drivers/ufs/upiu/upiu_transactions.h"
#include "unit-lib.h"

namespace ufs {
using namespace ufs_mock_device;

class ServerTest : public UfsTest {
 public:
  fidl::ClientEnd<fuchsia_hardware_ufs::Ufs> GetClient() {
    zx::result device = driver_test().Connect<fuchsia_hardware_ufs::Service::Device>();
    EXPECT_EQ(ZX_OK, device.status_value());
    return std::move(device.value());
  }

  fdf::Arena arena_{'test'};
};

TEST_F(ServerTest, ReadDescriptor) {
  zx::result result = driver_test().RunOnBackgroundDispatcherSync(
      [client_end = GetClient(), &mock_device = mock_device_]() {
        fidl::Arena arena;
        auto desc = fuchsia_hardware_ufs::wire::Descriptor::Builder(arena)
                        .type(fuchsia_hardware_ufs::DescriptorType::kDevice)
                        .Build();

        const fidl::WireResult result = fidl::WireCall(client_end)->ReadDescriptor(desc);
        ASSERT_TRUE(result.ok());
        const fit::result response = result.value();
        ASSERT_TRUE(response.is_ok());

        DeviceDescriptor descriptor;
        std::memset(&descriptor, 0, sizeof(DeviceDescriptor));
        auto response_data = response->data;
        std::memcpy(&descriptor, response_data.data(), sizeof(DeviceDescriptor));

        auto mock_desc = mock_device.GetDeviceDesc();
        ASSERT_EQ(descriptor.bLength, mock_desc.bLength);
        ASSERT_EQ(descriptor.bDescriptorIDN, mock_desc.bDescriptorIDN);
        ASSERT_EQ(descriptor.bDeviceSubClass, mock_desc.bDeviceSubClass);
        ASSERT_EQ(descriptor.bNumberWLU, mock_desc.bNumberWLU);
        ASSERT_EQ(descriptor.bInitPowerMode, mock_desc.bInitPowerMode);
        ASSERT_EQ(descriptor.bHighPriorityLUN, mock_desc.bHighPriorityLUN);
        ASSERT_EQ(descriptor.wSpecVersion, mock_desc.wSpecVersion);
        ASSERT_EQ(descriptor.bUD0BaseOffset, mock_desc.bUD0BaseOffset);
        ASSERT_EQ(descriptor.bUDConfigPLength, mock_desc.bUDConfigPLength);
      });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, WriteDescriptor) {
  zx::result result = driver_test().RunOnBackgroundDispatcherSync(
      [client_end = GetClient(), &mock_device = mock_device_]() {
        // 0x7F: Same priority for all LUNs
        // If all LUNs already have the same priority, change the priority of LUN0 higher
        // If specific LUN has higher priority, change to the same priority for all LUNs
        const uint8_t kLunPrioritySameForAll = 0x7F;
        uint8_t high_priority_lun =
            mock_device.GetDeviceDesc().bHighPriorityLUN == kLunPrioritySameForAll
                ? 0
                : kLunPrioritySameForAll;

        ConfigurationDescriptor descriptor;
        std::memset(&descriptor, 0, sizeof(ConfigurationDescriptor));
        descriptor.bLength = 0xE6;
        descriptor.bDescriptorIDN = 0x01;
        descriptor.bConfDescContinue = 0x00;
        descriptor.bHighPriorityLUN = high_priority_lun;

        fidl::Arena arena;
        auto desc = fuchsia_hardware_ufs::wire::Descriptor::Builder(arena)
                        .type(fuchsia_hardware_ufs::DescriptorType::kConfiguration)
                        .Build();
        std::vector<uint8_t> data_segment(sizeof(ConfigurationDescriptor));
        std::memcpy(data_segment.data(), &descriptor, sizeof(ConfigurationDescriptor));

        const fidl::WireResult result =
            fidl::WireCall(client_end)
                ->WriteDescriptor(desc, fidl::VectorView<uint8_t>(arena, data_segment));
        ASSERT_TRUE(result.ok());
        const fit::result response = result.value();
        ASSERT_TRUE(response.is_ok());
        ASSERT_EQ(high_priority_lun, mock_device.GetDeviceDesc().bHighPriorityLUN);
      });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, ReadFlag) {
  bool device_init = false;
  mock_device_.SetFlag(Flags::fDeviceInit, device_init);
  auto result = driver_test().RunOnBackgroundDispatcherSync(
      [client_end = GetClient(), &mock_device = mock_device_]() {
        fidl::Arena arena;
        auto flag = fuchsia_hardware_ufs::wire::Flag::Builder(arena)
                        .type(fuchsia_hardware_ufs::FlagType::kDeviceInit)
                        .Build();

        const fidl::WireResult result = fidl::WireCall(client_end)->ReadFlag(flag);
        ASSERT_TRUE(result.ok());
        const fit::result response = result.value();
        ASSERT_TRUE(response.is_ok());
        ASSERT_EQ(response->value, mock_device.GetFlag(Flags::fDeviceInit));
      });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, SetFlag) {
  bool refresh_enable = false;
  mock_device_.SetFlag(Flags::fRefreshEnable, refresh_enable);
  zx::result result = driver_test().RunOnBackgroundDispatcherSync(
      [client_end = GetClient(), &mock_device = mock_device_]() {
        fidl::Arena arena;
        auto flag = fuchsia_hardware_ufs::wire::Flag::Builder(arena)
                        .type(fuchsia_hardware_ufs::FlagType::kRefreshEnable)
                        .Build();

        const fidl::WireResult result = fidl::WireCall(client_end)->SetFlag(flag);
        ASSERT_TRUE(result.ok());
        const fit::result response = result.value();
        ASSERT_TRUE(response.is_ok());
        ASSERT_TRUE(response->value);
        ASSERT_EQ(response->value, mock_device.GetFlag(Flags::fRefreshEnable));
      });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, ToggleFlag) {
  bool refresh_enable = true;
  mock_device_.SetFlag(Flags::fRefreshEnable, refresh_enable);
  zx::result result = driver_test().RunOnBackgroundDispatcherSync(
      [client_end = GetClient(), &mock_device = mock_device_]() {
        fidl::Arena arena;
        auto flag = fuchsia_hardware_ufs::wire::Flag::Builder(arena)
                        .type(fuchsia_hardware_ufs::FlagType::kRefreshEnable)
                        .Build();

        const fidl::WireResult result = fidl::WireCall(client_end)->ToggleFlag(flag);
        ASSERT_TRUE(result.ok());
        const fit::result response = result.value();
        ASSERT_TRUE(response.is_ok());
        ASSERT_FALSE(response->value);
        ASSERT_EQ(response->value, mock_device.GetFlag(Flags::fRefreshEnable));
      });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, ClearFlag) {
  bool refresh_enable = true;
  mock_device_.SetFlag(Flags::fRefreshEnable, refresh_enable);
  zx::result result = driver_test().RunOnBackgroundDispatcherSync(
      [client_end = GetClient(), &mock_device = mock_device_]() {
        fidl::Arena arena;
        auto flag = fuchsia_hardware_ufs::wire::Flag::Builder(arena)
                        .type(fuchsia_hardware_ufs::FlagType::kRefreshEnable)
                        .Build();

        const fidl::WireResult result = fidl::WireCall(client_end)->ClearFlag(flag);
        ASSERT_TRUE(result.ok());
        const fit::result response = result.value();
        ASSERT_TRUE(response.is_ok());
        ASSERT_FALSE(response->value);
        ASSERT_EQ(response->value, mock_device.GetFlag(Flags::fRefreshEnable));
      });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, ReadAttribute) {
  mock_device_.SetAttribute(Attributes::bCurrentPowerMode,
                            static_cast<uint32_t>(UfsPowerMode::kActive));
  zx::result result = driver_test().RunOnBackgroundDispatcherSync(
      [client_end = GetClient(), &mock_device = mock_device_]() {
        fidl::Arena arena;
        auto attr = fuchsia_hardware_ufs::wire::Attribute::Builder(arena)
                        .type(fuchsia_hardware_ufs::AttributeType::kCurrentPowerMode)
                        .Build();
        const fidl::WireResult result = fidl::WireCall(client_end)->ReadAttribute(attr);
        ASSERT_TRUE(result.ok());
        const fit::result response = result.value();
        ASSERT_TRUE(response.is_ok());
        ASSERT_EQ(response->value, mock_device.GetAttribute(Attributes::bCurrentPowerMode));
      });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, WriteAttribute) {
  zx::result result = driver_test().RunOnBackgroundDispatcherSync([client_end = GetClient(),
                                                                   &mock_device = mock_device_]() {
    fidl::Arena arena;
    auto attr = fuchsia_hardware_ufs::wire::Attribute::Builder(arena)
                    .type(fuchsia_hardware_ufs::AttributeType::kConfigDescrLock)
                    .Build();

    uint32_t changed_value = 0x01;
    const fidl::WireResult result = fidl::WireCall(client_end)->WriteAttribute(attr, changed_value);
    ASSERT_TRUE(result.ok());

    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok());
    ASSERT_EQ(changed_value, mock_device.GetAttribute(Attributes::bConfigDescrLock));
  });

  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, UicCommandDmeGet) {
  zx::result result = driver_test().RunOnBackgroundDispatcherSync([client_end = GetClient()]() {
    auto uic_arg_mib_sel = [](uint16_t attr, uint16_t sel) {
      return (((attr) & 0xFFFF) << 16) | ((sel) & 0xFFFF);
    };

    fidl::Array<uint32_t, fuchsia_hardware_ufs::kUicCommandArgumentCount> args;
    args[0] = uic_arg_mib_sel(PA_MaxRxHSGear, 0);
    const fidl::WireResult result =
        fidl::WireCall(client_end)
            ->SendUicCommand(fuchsia_hardware_ufs::UicCommandOpcode::kDmeGet, args);
    ASSERT_TRUE(result.ok());

    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok());
    ASSERT_EQ(response->result, ufs_mock_device::kMaxGear);
  });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, UicCommandDmeSet) {
  zx::result result = driver_test().RunOnBackgroundDispatcherSync([client_end = GetClient()]() {
    auto uic_arg_mib_sel = [](uint16_t attr, uint16_t sel) {
      return (((attr) & 0xFFFF) << 16) | ((sel) & 0xFFFF);
    };

    fidl::Array<uint32_t, fuchsia_hardware_ufs::kUicCommandArgumentCount> args;
    args[0] = uic_arg_mib_sel(PA_MaxRxHSGear, 0);
    const fidl::WireResult result =
        fidl::WireCall(client_end)
            ->SendUicCommand(fuchsia_hardware_ufs::UicCommandOpcode::kDmeSet, args);
    ASSERT_TRUE(result.ok());

    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok());
    ASSERT_EQ(response->result, 0U);
  });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, UicCommandDmePeerGet) {
  zx::result result = driver_test().RunOnBackgroundDispatcherSync([client_end = GetClient()]() {
    auto uic_arg_mib_sel = [](uint16_t attr, uint16_t sel) {
      return (((attr) & 0xFFFF) << 16) | ((sel) & 0xFFFF);
    };

    fidl::Array<uint32_t, fuchsia_hardware_ufs::kUicCommandArgumentCount> args;
    args[0] = uic_arg_mib_sel(PA_TActivate, 0);
    const fidl::WireResult result =
        fidl::WireCall(client_end)
            ->SendUicCommand(fuchsia_hardware_ufs::UicCommandOpcode::kDmePeerGet, args);
    ASSERT_TRUE(result.ok());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok());
    ASSERT_EQ(response->result, ufs_mock_device::kTActivate);
  });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, UicCommandDmePeerSet) {
  zx::result result = driver_test().RunOnBackgroundDispatcherSync([client_end = GetClient()]() {
    auto uic_arg_mib_sel = [](uint16_t attr, uint16_t sel) {
      return (((attr) & 0xFFFF) << 16) | ((sel) & 0xFFFF);
    };

    fidl::Array<uint32_t, fuchsia_hardware_ufs::kUicCommandArgumentCount> args;
    args[0] = uic_arg_mib_sel(PA_TActivate, 0);
    const fidl::WireResult result =
        fidl::WireCall(client_end)
            ->SendUicCommand(fuchsia_hardware_ufs::UicCommandOpcode::kDmePeerSet, args);
    ASSERT_TRUE(result.ok());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_ok());
    ASSERT_EQ(response->result, 0U);
  });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, RequestQueryUpiu) {
  uint8_t power_mode = static_cast<uint8_t>(UfsPowerMode::kActive);
  mock_device_.SetAttribute(Attributes::bCurrentPowerMode, power_mode);
  zx::result result = driver_test().RunOnBackgroundDispatcherSync(
      [client_end = GetClient(), &mock_device = mock_device_, &arena = arena_]() {
        // Prepare the request upiu
        QueryRequestUpiuData request_upiu;
        request_upiu.header.trans_type = UpiuTransactionCodes::kQueryRequest;
        request_upiu.header.function = static_cast<uint8_t>(QueryFunction::kStandardReadRequest);
        request_upiu.opcode = static_cast<uint8_t>(QueryOpcode::kReadAttribute);
        request_upiu.idn = 0x2;

        // Copy the data into a vector for FIDL transmission
        std::vector<uint8_t> upiu_vector(sizeof(QueryRequestUpiuData));
        std::memcpy(upiu_vector.data(), &request_upiu, sizeof(QueryRequestUpiuData));
        fidl::VectorView<uint8_t> upiu{arena, upiu_vector};

        // Send the request
        auto response = fidl::WireCall(client_end)->Request(upiu);
        ASSERT_TRUE(response.ok());
        ASSERT_TRUE(response->is_ok());

        // Check response data
        auto response_data = response.value()->response;
        QueryResponseUpiuData response_upiu;
        std::memcpy(&response_upiu, response_data.data(), sizeof(QueryResponseUpiuData));

        // Validate the response
        ASSERT_EQ(response_upiu.header.trans_type, UpiuTransactionCodes::kQueryResponse);
        ASSERT_EQ(response_upiu.header.function,
                  static_cast<uint8_t>(QueryFunction::kStandardReadRequest));
        ASSERT_EQ(response_upiu.header.response, UpiuHeaderResponseCode::kTargetSuccess);
        ASSERT_EQ(response_upiu.opcode, static_cast<uint8_t>(QueryOpcode::kReadAttribute));
        ASSERT_EQ(response_upiu.idn, static_cast<uint8_t>(Attributes::bCurrentPowerMode));
        ASSERT_EQ(betoh32(response_upiu.value),
                  mock_device.GetAttribute(Attributes::bCurrentPowerMode));
      });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, ReadDescriptorWithInvalidIdn) {
  zx::result result = driver_test().RunOnBackgroundDispatcherSync([client_end = GetClient()]() {
    // Checks if an error occurs when the request type is missing.
    fidl::Arena arena;
    auto desc = fuchsia_hardware_ufs::wire::Descriptor::Builder(arena).Build();
    const fidl::WireResult result = fidl::WireCall(client_end)->ReadDescriptor(desc);
    ASSERT_TRUE(result.ok());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_error());
    ASSERT_EQ(response.error_value(), fuchsia_hardware_ufs::QueryErrorCode::kInvalidIdn);
  });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, RequestInternalFail) {
  // Reserve admin slot.
  ASSERT_OK(ReserveAdminSlot());
  zx::result result = driver_test().RunOnBackgroundDispatcherSync([client_end = GetClient()]() {
    fidl::Arena arena;
    auto attr = fuchsia_hardware_ufs::wire::Attribute::Builder(arena)
                    .type(fuchsia_hardware_ufs::AttributeType::kCurrentPowerMode)
                    .Build();
    const fidl::WireResult result = fidl::WireCall(client_end)->ReadAttribute(attr);
    ASSERT_TRUE(result.ok());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_error());
    ASSERT_EQ(response.error_value(), fuchsia_hardware_ufs::QueryErrorCode::kGeneralFailure);
  });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, RequestNotSupportedTransaction) {
  zx::result result =
      driver_test().RunOnBackgroundDispatcherSync([client_end = GetClient(), &arena = arena_]() {
        // Prepare the request upiu
        QueryRequestUpiuData request_upiu;
        request_upiu.header.trans_type = UpiuTransactionCodes::kRejectUpiu;

        // Copy the data into a vector for FIDL transmission
        std::vector<uint8_t> upiu_vector(sizeof(QueryRequestUpiuData));
        std::memcpy(upiu_vector.data(), &request_upiu, sizeof(QueryRequestUpiuData));
        fidl::VectorView<uint8_t> upiu{arena, upiu_vector};

        // Send the request
        const fidl::WireResult result = fidl::WireCall(client_end)->Request(upiu);
        ASSERT_TRUE(result.ok());
        const fit::result response = result.value();
        ASSERT_TRUE(response.is_error());
        ASSERT_EQ(response.error_value(), ZX_ERR_NOT_SUPPORTED);
      });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, RequestWrongUpiuData) {
  zx::result result =
      driver_test().RunOnBackgroundDispatcherSync([client_end = GetClient(), &arena = arena_]() {
        // Prepare the request upiu
        CommandUpiuData request_upiu;
        request_upiu.header.trans_type = UpiuTransactionCodes::kQueryRequest;
        request_upiu.header.function = static_cast<uint8_t>(QueryFunction::kStandardReadRequest);

        // Copy the data into a vector for FIDL transmission
        std::vector<uint8_t> upiu_vector(sizeof(CommandUpiuData));
        std::memcpy(upiu_vector.data(), &request_upiu, sizeof(CommandUpiuData));
        fidl::VectorView<uint8_t> upiu{arena, upiu_vector};

        // Send the request
        const fidl::WireResult result = fidl::WireCall(client_end)->Request(upiu);
        ASSERT_TRUE(result.ok());
        const fit::result response = result.value();
        ASSERT_TRUE(response.is_error());
        ASSERT_EQ(response.error_value(), ZX_ERR_INVALID_ARGS);
      });
  ASSERT_OK(result.status_value());
}

TEST_F(ServerTest, SendUicCommandWithUnsupportedOpcode) {
  zx::result result = driver_test().RunOnBackgroundDispatcherSync([client_end = GetClient()]() {
    fidl::Array<uint32_t, fuchsia_hardware_ufs::kUicCommandArgumentCount> args{0};
    const fidl::WireResult result =
        fidl::WireCall(client_end)
            ->SendUicCommand(fuchsia_hardware_ufs::UicCommandOpcode::kDmeHiberEnter, args);
    ASSERT_TRUE(result.ok());
    const fit::result response = result.value();
    ASSERT_TRUE(response.is_error());
    ASSERT_EQ(response.error_value(), ZX_ERR_NOT_SUPPORTED);
  });
  ASSERT_OK(result.status_value());
}

}  // namespace ufs
