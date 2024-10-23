// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/nvme/nvme.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/driver/testing/cpp/minimal_compat_environment.h>
#include <lib/fake-bti/bti.h>
#include <lib/fpromise/single_threaded_executor.h>
#include <lib/inspect/testing/cpp/inspect.h>

#include <memory>

#include <gtest/gtest.h>

#include "src/devices/block/drivers/nvme/commands/nvme-io.h"
#include "src/devices/block/drivers/nvme/fake/fake-admin-commands.h"
#include "src/devices/block/drivers/nvme/fake/fake-controller.h"
#include "src/devices/block/drivers/nvme/fake/fake-namespace.h"
#include "src/lib/testing/predicates/status.h"

namespace nvme {

class TestNvme : public Nvme {
 public:
  // Modify to configure the behaviour of this test driver.
  static fake_nvme::FakeController controller_;

  TestNvme(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : Nvme(std::move(start_args), std::move(dispatcher)) {}

 protected:
  zx::result<fit::function<void()>> InitResources() override {
    pci_ = ddk::Pci{};
    mmio_ = controller_.registers().GetBuffer();

    // Create a fake BTI.
    zx::bti fake_bti;
    ZX_ASSERT(fake_bti_create(fake_bti.reset_and_get_address()) == ZX_OK);
    bti_ = std::move(fake_bti);

    // Set up an interrupt.
    irq_mode_ = fuchsia_hardware_pci::InterruptMode::kMsiX;
    auto irq = controller_.GetOrCreateInterrupt(0);
    ZX_ASSERT(irq.is_ok());
    irq_ = std::move(*irq);

    controller_.SetNvme(this);
    return zx::ok([] {});
  }

  fake_nvme::FakeAdminCommands admin_commands_{controller_};
};

fake_nvme::FakeController TestNvme::controller_;

class TestConfig final {
 public:
  using DriverType = TestNvme;
  using EnvironmentType = fdf_testing::MinimalCompatEnvironment;
};

class NvmeTest : public ::testing::Test {
 public:
  void StartDriver() {
    zx::result<> result = driver_test().StartDriver();
    ASSERT_OK(result);
  }

  void TearDown() override {
    zx::result<> result = driver_test().StopDriver();
    ASSERT_OK(result);
  }

  fdf_testing::ForegroundDriverTest<TestConfig>& driver_test() { return driver_test_; }

  void CheckStringPropertyPrefix(const inspect::NodeValue& node, const std::string& property,
                                 const char* expected) {
    const auto* actual = node.get_property<inspect::StringPropertyValue>(property);
    EXPECT_TRUE(actual);
    if (!actual) {
      return;
    }
    EXPECT_EQ(0, strncmp(actual->value().data(), expected, strlen(expected)));
  }

  void CheckBooleanProperty(const inspect::NodeValue& node, const std::string& property,
                            bool expected) {
    const auto* actual = node.get_property<inspect::BoolPropertyValue>(property);
    EXPECT_TRUE(actual);
    if (!actual) {
      return;
    }
    EXPECT_EQ(actual->value(), expected);
  }

 protected:
  fdf_testing::ForegroundDriverTest<TestConfig> driver_test_;
};

TEST_F(NvmeTest, BasicTest) {
  ASSERT_NO_FATAL_FAILURE(StartDriver());
  fpromise::result<inspect::Hierarchy> hierarchy =
      fpromise::run_single_threaded(inspect::ReadFromInspector(driver_test().driver()->inspect()));
  ASSERT_TRUE(hierarchy.is_ok());
  const auto* nvme = hierarchy.value().GetByPath({"nvme"});
  ASSERT_NE(nullptr, nvme);
  const auto* controller = nvme->GetByPath({"controller"});
  ASSERT_NE(nullptr, controller);
  CheckStringPropertyPrefix(controller->node(), "model_number",
                            fake_nvme::FakeAdminCommands::kModelNumber);
  CheckStringPropertyPrefix(controller->node(), "serial_number",
                            fake_nvme::FakeAdminCommands::kSerialNumber);
  CheckStringPropertyPrefix(controller->node(), "firmware_rev",
                            fake_nvme::FakeAdminCommands::kFirmwareRev);
  CheckBooleanProperty(controller->node(), "volatile_write_cache_enabled", true);
}

TEST_F(NvmeTest, NamespaceBlockInfo) {
  fake_nvme::FakeNamespace fake_ns;
  TestNvme::controller_.AddNamespace(1, fake_ns);
  ASSERT_NO_FATAL_FAILURE(StartDriver());

  ASSERT_GT(driver_test().driver()->namespaces().size(), 0u);
  Namespace* ns = driver_test().driver()->namespaces()[0].get();

  block_info_t info;
  uint64_t op_size;
  ns->BlockImplQuery(&info, &op_size);
  EXPECT_EQ(512u, info.block_size);
  EXPECT_EQ(1024u, info.block_count);
  EXPECT_TRUE(info.flags & FLAG_FUA_SUPPORT);
}

TEST_F(NvmeTest, NamespaceReadTest) {
  fake_nvme::FakeNamespace fake_ns;
  TestNvme::controller_.AddNamespace(1, fake_ns);
  TestNvme::controller_.AddIoCommand(
      nvme::IoCommandOpcode::kRead,
      [](nvme::Submission& submission, const nvme::TransactionData& data,
         nvme::Completion& completion) {
        completion.set_status_code_type(nvme::StatusCodeType::kGeneric)
            .set_status_code(nvme::GenericStatus::kSuccess);
      });
  ASSERT_NO_FATAL_FAILURE(StartDriver());

  ASSERT_GT(driver_test().driver()->namespaces().size(), 0u);
  Namespace* ns = driver_test().driver()->namespaces()[0].get();

  block_info_t info;
  uint64_t op_size;
  ns->BlockImplQuery(&info, &op_size);

  sync_completion_t done;
  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));
  auto block_op = std::make_unique<uint8_t[]>(op_size);
  auto op = reinterpret_cast<block_op_t*>(block_op.get());
  *op = {.rw = {
             .command = {.opcode = BLOCK_OPCODE_READ, .flags = 0},
             .vmo = vmo.get(),
             .length = 1,
             .offset_dev = 0,
             .offset_vmo = 0,
         }};
  ns->BlockImplQueue(op, callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);
}

}  // namespace nvme

FUCHSIA_DRIVER_EXPORT(nvme::TestNvme);
