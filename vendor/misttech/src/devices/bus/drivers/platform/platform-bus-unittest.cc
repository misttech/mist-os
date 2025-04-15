// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "src/devices/bus/drivers/platform/platform-bus.h"

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/fake-bti/bti.h>
#include <lib/zbi-format/zbi.h>
#include <zircon/status.h>

#include <ddk/metadata/test.h>
#include <gtest/gtest.h>

#include "lib/fidl/cpp/wire/channel.h"
#include "src/devices/bus/drivers/platform/node-util.h"
#include "src/devices/bus/drivers/platform/platform_bus_config.h"

namespace {

class BootItems final : public fidl::WireServer<fuchsia_boot::Items> {
 public:
  fidl::ProtocolHandler<fuchsia_boot::Items> handler(async_dispatcher_t* dispatcher) {
    return bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure);
  }

  void Get(GetRequestView request, GetCompleter::Sync& completer) override;
  void Get2(Get2RequestView request, Get2Completer::Sync& completer) override;
  void GetBootloaderFile(GetBootloaderFileRequestView request,
                         GetBootloaderFileCompleter::Sync& completer) override;

 private:
  fidl::ServerBindingGroup<fuchsia_boot::Items> bindings_;
};

const zbi_platform_id_t kPlatformId = []() {
  zbi_platform_id_t plat_id = {};
  plat_id.vid = PDEV_VID_TEST;
  plat_id.pid = PDEV_PID_PBUS_TEST;
  strcpy(plat_id.board_name, "pbus-unit-test");
  return plat_id;
}();

#define BOARD_REVISION_TEST 42

const zbi_board_info_t kBoardInfo = []() {
  zbi_board_info_t board_info = {};
  board_info.revision = BOARD_REVISION_TEST;
  return board_info;
}();

zx_status_t GetBootItem(const std::vector<board_test::DeviceEntry>& entries, uint32_t type,
                        uint32_t extra, zx::vmo* out, uint32_t* length) {
  zx::vmo vmo;
  switch (type) {
    case ZBI_TYPE_PLATFORM_ID: {
      zx_status_t status = zx::vmo::create(sizeof(kPlatformId), 0, &vmo);
      if (status != ZX_OK) {
        return status;
      }
      status = vmo.write(&kPlatformId, 0, sizeof(kPlatformId));
      if (status != ZX_OK) {
        return status;
      }
      *length = sizeof(kPlatformId);
      break;
    }
    case ZBI_TYPE_DRV_BOARD_INFO: {
      zx_status_t status = zx::vmo::create(sizeof(kBoardInfo), 0, &vmo);
      if (status != ZX_OK) {
        return status;
      }
      status = vmo.write(&kBoardInfo, 0, sizeof(kBoardInfo));
      if (status != ZX_OK) {
        return status;
      }
      *length = sizeof(kBoardInfo);
      break;
    }
    case ZBI_TYPE_DRV_BOARD_PRIVATE: {
      size_t list_size = sizeof(board_test::DeviceList);
      size_t entry_size = entries.size() * sizeof(board_test::DeviceEntry);

      size_t metadata_size = 0;
      for (const board_test::DeviceEntry& entry : entries) {
        metadata_size += entry.metadata_size;
      }

      zx_status_t status = zx::vmo::create(list_size + entry_size + metadata_size, 0, &vmo);
      if (status != ZX_OK) {
        return status;
      }

      // Write DeviceList to vmo.
      board_test::DeviceList list{.count = entries.size()};
      status = vmo.write(&list, 0, sizeof(list));
      if (status != ZX_OK) {
        return status;
      }

      // Write DeviceEntries to vmo.
      status = vmo.write(entries.data(), list_size, entry_size);
      if (status != ZX_OK) {
        return status;
      }

      // Write Metadata to vmo.
      size_t write_offset = list_size + entry_size;
      for (const board_test::DeviceEntry& entry : entries) {
        status = vmo.write(entry.metadata, write_offset, entry.metadata_size);
        if (status != ZX_OK) {
          return status;
        }
        write_offset += entry.metadata_size;
      }

      *length = static_cast<uint32_t>(list_size + entry_size + metadata_size);
      break;
    }
    default:
      return ZX_ERR_NOT_FOUND;
  }
  *out = std::move(vmo);
  return ZX_OK;
}

void BootItems::Get(GetRequestView request, GetCompleter::Sync& completer) {
  zx::vmo vmo;
  uint32_t length = 0;
  std::vector<board_test::DeviceEntry> entries = {};
  std::ignore = GetBootItem(entries, request->type, request->extra, &vmo, &length);
  completer.Reply(std::move(vmo), length);
}

void BootItems::Get2(Get2RequestView request, Get2Completer::Sync& completer) {
  std::vector<board_test::DeviceEntry> entries = {};
  zx::vmo vmo;
  uint32_t length = 0;
  uint32_t extra = 0;
  zx_status_t status = GetBootItem(entries, request->type, extra, &vmo, &length);
  if (status != ZX_OK) {
    completer.Reply(zx::error(status));
    return;
  }
  std::vector<fuchsia_boot::wire::RetrievedItems> result;
  fuchsia_boot::wire::RetrievedItems items = {
      .payload = std::move(vmo), .length = length, .extra = extra};
  result.emplace_back(std::move(items));
  completer.ReplySuccess(
      fidl::VectorView<fuchsia_boot::wire::RetrievedItems>::FromExternal(result));
}

void BootItems::GetBootloaderFile(GetBootloaderFileRequestView request,
                                  GetBootloaderFileCompleter::Sync& completer) {
  completer.Reply(zx::vmo());
}

class TestEnvironment : public fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    auto dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();
    return to_driver_vfs.component().AddUnmanagedProtocol<fuchsia_boot::Items>(
        fake_boot_items_.handler(dispatcher));
  }

 private:
  BootItems fake_boot_items_;
};

class TestConfig final {
 public:
  using DriverType = platform_bus::PlatformBus;
  using EnvironmentType = TestEnvironment;
};

class PlatformBusTest : public ::testing::Test {
 public:
  void SetUp() override {
    zx::result<> result =
        driver_test().StartDriverWithCustomStartArgs([](fdf::DriverStartArgs& args) {
          platform_bus_config::Config config;
          args.config(config.ToVmo());
        });
    ASSERT_EQ(ZX_OK, result.status_value());
  }
  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  fdf_testing::ForegroundDriverTest<TestConfig>& driver_test() { return driver_test_; }

 private:
  fdf_testing::ForegroundDriverTest<TestConfig> driver_test_;
};

uint32_t g_bti_created = 0;

TEST_F(PlatformBusTest, IommuGetBti) {
  g_bti_created = 0;

  auto& pbus = *driver_test().driver();
  EXPECT_EQ(g_bti_created, 0u);
  ASSERT_EQ(ZX_OK, pbus.GetBti(0, 0).status_value());
  EXPECT_EQ(g_bti_created, 1u);
  ASSERT_EQ(ZX_OK, pbus.GetBti(0, 0).status_value());
  EXPECT_EQ(g_bti_created, 1u);
  ASSERT_EQ(ZX_OK, pbus.GetBti(0, 1).status_value());
  EXPECT_EQ(g_bti_created, 2u);
}

TEST(PlatformBusTest2, GetMmioIndex) {
  const std::vector<fuchsia_hardware_platform_bus::Mmio> mmios{
      {{
          .base = 1,
          .length = 2,
          .name = "first",
      }},
      {{
          .base = 3,
          .length = 4,
          .name = "second",
      }},
  };

  fuchsia_hardware_platform_bus::Node node = {};
  node.mmio() = mmios;

  {
    auto result = platform_bus::GetMmioIndex(node, "first");
    ASSERT_TRUE(result.has_value());
    ASSERT_EQ(result.value(), 0u);
  }
  {
    auto result = platform_bus::GetMmioIndex(node, "second");
    ASSERT_TRUE(result.has_value());
    ASSERT_EQ(result.value(), 1u);
  }
  {
    auto result = platform_bus::GetMmioIndex(node, "none");
    ASSERT_FALSE(result.has_value());
  }
}

TEST(PlatformBusTest2, GetMmioIndexNoMmios) {
  fuchsia_hardware_platform_bus::Node node = {};
  {
    auto result = platform_bus::GetMmioIndex(node, "none");
    ASSERT_FALSE(result.has_value());
  }
}

}  // namespace

__EXPORT
zx_status_t zx_bti_create(zx_handle_t handle, uint32_t options, uint64_t bti_id, zx_handle_t* out) {
  g_bti_created++;
  return fake_bti_create(out);
}
