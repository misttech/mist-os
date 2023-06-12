// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/nvme/nvme.h"

#include <lib/fake-bti/bti.h>
#include <lib/inspect/testing/cpp/zxtest/inspect.h>

#include <memory>

#include <zxtest/zxtest.h>

#include "src/devices/block/drivers/nvme/commands/nvme-io.h"
#include "src/devices/block/drivers/nvme/fake/fake-admin-commands.h"
#include "src/devices/block/drivers/nvme/fake/fake-controller.h"
#include "src/devices/block/drivers/nvme/fake/fake-namespace.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace nvme {

class NvmeTest : public inspect::InspectTestHelper, public zxtest::Test {
 public:
  void SetUp() override {
    fake_root_ = MockDevice::FakeRootParent();

    // Create a fake BTI.
    zx::bti fake_bti;
    ASSERT_OK(fake_bti_create(fake_bti.reset_and_get_address()));

    // Set up an interrupt.
    auto irq = controller_.GetOrCreateInterrupt(0);
    ASSERT_OK(irq.status_value());

    // Set up the driver.
    auto driver = std::make_unique<Nvme>(
        fake_root_.get(), ddk::Pci{}, controller_.registers().GetBuffer(),
        fuchsia_hardware_pci::InterruptMode::kMsiX, std::move(*irq), std::move(fake_bti));
    ASSERT_OK(driver->AddDevice());
    [[maybe_unused]] auto unused = driver.release();

    device_ = fake_root_->GetLatestChild();
    nvme_ = device_->GetDeviceContext<Nvme>();
    controller_.SetNvme(nvme_);
  }

  void RunInit() {
    device_->InitOp();
    ASSERT_OK(device_->WaitUntilInitReplyCalled(zx::time::infinite()));
    ASSERT_OK(device_->InitReplyCallStatus());
  }

  void TearDown() override {
    device_async_remove(device_);
    EXPECT_OK(mock_ddk::ReleaseFlaggedDevices(device_));
  }

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
  std::shared_ptr<zx_device> fake_root_;
  zx_device* device_;
  Nvme* nvme_;
  fake_nvme::FakeController controller_;
  fake_nvme::FakeAdminCommands admin_commands_{controller_};
};

TEST_F(NvmeTest, BasicTest) {
  ASSERT_NO_FATAL_FAILURE(RunInit());
  ASSERT_NO_FATAL_FAILURE(ReadInspect(nvme_->inspector().DuplicateVmo()));
  const auto* nvme = hierarchy().GetByPath({"nvme"});
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
  fake_nvme::FakeNamespace ns;
  controller_.AddNamespace(1, ns);
  ASSERT_NO_FATAL_FAILURE(RunInit());
  while (device_->child_count() == 0) {
    zx::nanosleep(zx::deadline_after(zx::msec(1)));
  }

  zx_device* ns_dev = device_->GetLatestChild();
  ns_dev->InitOp();
  ns_dev->WaitUntilInitReplyCalled(zx::time::infinite());
  ASSERT_OK(ns_dev->InitReplyCallStatus());

  ddk::BlockImplProtocolClient client(ns_dev);
  block_info_t info;
  uint64_t op_size;
  client.Query(&info, &op_size);
  EXPECT_EQ(512, info.block_size);
  EXPECT_EQ(1024, info.block_count);
  EXPECT_TRUE(info.flags & FLAG_FUA_SUPPORT);
}

TEST_F(NvmeTest, NamespaceReadTest) {
  fake_nvme::FakeNamespace ns;
  controller_.AddNamespace(1, ns);
  controller_.AddIoCommand(nvme::IoCommandOpcode::kRead,
                           [](nvme::Submission& submission, const nvme::TransactionData& data,
                              nvme::Completion& completion) {
                             completion.set_status_code_type(nvme::StatusCodeType::kGeneric)
                                 .set_status_code(nvme::GenericStatus::kSuccess);
                           });
  ASSERT_NO_FATAL_FAILURE(RunInit());
  while (device_->child_count() == 0) {
    zx::nanosleep(zx::deadline_after(zx::msec(1)));
  }

  zx_device* ns_dev = device_->GetLatestChild();
  ns_dev->InitOp();
  ns_dev->WaitUntilInitReplyCalled(zx::time::infinite());
  ASSERT_OK(ns_dev->InitReplyCallStatus());

  ddk::BlockImplProtocolClient client(ns_dev);
  block_info_t info;
  uint64_t op_size;
  client.Query(&info, &op_size);

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
  client.Queue(op, callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);
}

}  // namespace nvme
