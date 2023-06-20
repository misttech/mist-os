// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "helper/platform_msd_device_helper.h"
#include "magma_util/short_macros.h"
#include "src/graphics/drivers/msd-arm-mali/include/magma_arm_mali_types.h"
#include "src/graphics/lib/magma/src/magma_util/platform/platform_semaphore.h"
#include "sys_driver/magma_driver.h"
#include "sys_driver/magma_system_connection.h"
#include "sys_driver/magma_system_context.h"

namespace {

class Test {
 public:
  void TestValidImmediate() {
    auto ctx = InitializeContext();
    ASSERT_TRUE(ctx);

    magma_arm_mali_atom atom[2];
    atom[0].size = sizeof(atom[0]);
    atom[0].atom_number = 1;
    atom[0].flags = 1;
    atom[0].dependencies[0].atom_number = 0;
    atom[0].dependencies[1].atom_number = 0;
    atom[1].size = sizeof(atom[1]);
    atom[1].atom_number = 2;
    atom[1].flags = 1;
    atom[1].dependencies[0].atom_number = 1;
    atom[1].dependencies[0].type = kArmMaliDependencyOrder;
    atom[1].dependencies[1].atom_number = 0;

    magma::Status status = ctx->ExecuteImmediateCommands(sizeof(atom), &atom, 0, nullptr);
    EXPECT_EQ(MAGMA_STATUS_OK, status.get());
  }

  void TestValidLarger() {
    auto ctx = InitializeContext();
    ASSERT_TRUE(ctx);

    uint8_t buffer[100];
    ASSERT_GT(sizeof(buffer), sizeof(magma_arm_mali_atom));
    auto atom = reinterpret_cast<magma_arm_mali_atom*>(buffer);
    atom->size = sizeof(buffer);
    atom->atom_number = 1;
    atom->flags = 1;
    atom->dependencies[0].atom_number = 0;
    atom->dependencies[1].atom_number = 0;

    magma::Status status = ctx->ExecuteImmediateCommands(sizeof(buffer), buffer, 0, nullptr);
    EXPECT_EQ(MAGMA_STATUS_OK, status.get());
  }

  void TestInvalidTooLarge() {
    auto ctx = InitializeContext();
    ASSERT_TRUE(ctx);

    uint8_t buffer[100];
    ASSERT_GT(sizeof(buffer), sizeof(magma_arm_mali_atom) + 1);
    auto atom = reinterpret_cast<magma_arm_mali_atom*>(buffer);
    atom->size = sizeof(buffer);
    atom->atom_number = 1;
    atom->flags = 1;
    atom->dependencies[0].atom_number = 0;
    atom->dependencies[1].atom_number = 0;

    magma::Status status = ctx->ExecuteImmediateCommands(sizeof(buffer) - 1, buffer, 0, nullptr);
    EXPECT_EQ(MAGMA_STATUS_CONTEXT_KILLED, status.get());
  }

  void TestInvalidOverflow() {
    auto ctx = InitializeContext();
    ASSERT_TRUE(ctx);

    magma_arm_mali_atom atom[3];
    for (uint8_t i = 0; i < 3; i++) {
      atom[i].size = sizeof(atom[i]);
      atom[i].atom_number = i + 1;
      atom[i].flags = 1;
      atom[i].dependencies[0].atom_number = 0;
      atom[i].dependencies[1].atom_number = 0;
    }
    atom[2].size = reinterpret_cast<uintptr_t>(&atom[1]) - reinterpret_cast<uintptr_t>(&atom[2]);
    EXPECT_GE(atom[2].size, static_cast<uint64_t>(INT64_MAX));

    magma::Status status = ctx->ExecuteImmediateCommands(sizeof(atom), atom, 0, nullptr);
    EXPECT_EQ(MAGMA_STATUS_CONTEXT_KILLED, status.get());
  }

  void TestInvalidZeroSize() {
    auto ctx = InitializeContext();
    ASSERT_TRUE(ctx);

    magma_arm_mali_atom atom;
    atom.size = 0;
    atom.atom_number = 1;
    atom.flags = 1;
    atom.dependencies[0].atom_number = 0;
    atom.dependencies[1].atom_number = 0;

    // Shouldn't infinite loop, for example.
    magma::Status status = ctx->ExecuteImmediateCommands(sizeof(atom), &atom, 0, nullptr);
    EXPECT_EQ(MAGMA_STATUS_CONTEXT_KILLED, status.get());
  }

  void TestInvalidSmaller() {
    auto ctx = InitializeContext();
    ASSERT_TRUE(ctx);

    magma_arm_mali_atom atom;
    atom.size = sizeof(atom) - 1;
    atom.atom_number = 1;
    atom.flags = 1;
    atom.dependencies[0].atom_number = 0;
    atom.dependencies[1].atom_number = 0;

    magma::Status status = ctx->ExecuteImmediateCommands(sizeof(atom) - 1, &atom, 0, nullptr);
    EXPECT_EQ(MAGMA_STATUS_CONTEXT_KILLED, status.get());
  }

  void TestInvalidInUse() {
    auto ctx = InitializeContext();
    ASSERT_TRUE(ctx);

    magma_arm_mali_atom atom[2];
    atom[0].size = sizeof(atom[0]);
    atom[0].atom_number = 0;
    atom[0].flags = 1;
    atom[0].dependencies[0].atom_number = 0;
    atom[0].dependencies[1].atom_number = 0;
    atom[1].size = sizeof(atom[1]);
    atom[1].atom_number = 0;
    atom[1].flags = 1;
    atom[1].dependencies[0].atom_number = 0;
    atom[1].dependencies[1].atom_number = 0;

    magma::Status status = ctx->ExecuteImmediateCommands(sizeof(atom), &atom, 0, nullptr);
    // There's no device thread, so the atoms shouldn't be able to complete.
    EXPECT_EQ(MAGMA_STATUS_CONTEXT_KILLED, status.get());
  }

  void TestInvalidDependencyNotSubmitted() {
    auto ctx = InitializeContext();
    ASSERT_TRUE(ctx);

    magma_arm_mali_atom atom;
    atom.size = sizeof(atom);
    atom.atom_number = 1;
    atom.flags = 1;
    // Can't depend on self or on later atoms.
    atom.dependencies[0].atom_number = 1;
    atom.dependencies[0].type = kArmMaliDependencyOrder;
    atom.dependencies[1].atom_number = 0;

    magma::Status status = ctx->ExecuteImmediateCommands(sizeof(atom), &atom, 0, nullptr);
    EXPECT_EQ(MAGMA_STATUS_CONTEXT_KILLED, status.get());
  }

  void TestInvalidDependencyType() {
    auto ctx = InitializeContext();
    ASSERT_TRUE(ctx);

    magma_arm_mali_atom atom[2];
    atom[0].size = sizeof(atom[0]);
    atom[0].atom_number = 1;
    atom[0].flags = 1;
    atom[0].dependencies[0].atom_number = 0;
    atom[0].dependencies[1].atom_number = 0;
    atom[1].size = sizeof(atom[1]);
    atom[1].atom_number = 2;
    atom[1].flags = 1;
    atom[1].dependencies[0].atom_number = 1;
    atom[1].dependencies[0].type = 5;
    atom[1].dependencies[1].atom_number = 0;

    magma::Status status = ctx->ExecuteImmediateCommands(sizeof(atom), &atom, 0, nullptr);
    EXPECT_EQ(MAGMA_STATUS_CONTEXT_KILLED, status.get());
  }

  void TestInvalidSemaphoreImmediate() {
    auto ctx = InitializeContext();
    ASSERT_TRUE(ctx);

    magma_arm_mali_atom atom;
    atom.size = sizeof(atom);
    atom.atom_number = 0;
    atom.flags = kAtomFlagSemaphoreSet;
    atom.dependencies[0].atom_number = 0;
    atom.dependencies[1].atom_number = 0;

    magma::Status status = ctx->ExecuteImmediateCommands(sizeof(atom), &atom, 0, nullptr);
    EXPECT_EQ(MAGMA_STATUS_CONTEXT_KILLED, status.get());
  }

  void TestSemaphoreImmediate() {
    auto ctx = InitializeContext();
    ASSERT_TRUE(ctx);
    auto platform_semaphore = magma::PlatformSemaphore::Create();
    zx::handle handle;
    platform_semaphore->duplicate_handle(&handle);
    connection_->ImportObject(std::move(handle), fuchsia_gpu_magma::wire::ObjectType::kEvent,
                              platform_semaphore->id());

    magma_arm_mali_atom atom;
    atom.size = sizeof(atom);
    atom.atom_number = 0;
    atom.flags = kAtomFlagSemaphoreSet;
    atom.dependencies[0].atom_number = 0;
    atom.dependencies[1].atom_number = 0;
    uint64_t semaphores[] = {platform_semaphore->id()};

    magma::Status status = ctx->ExecuteImmediateCommands(sizeof(atom), &atom, 1, semaphores);
    EXPECT_EQ(MAGMA_STATUS_OK, status.get());
  }

 private:
  msd::MagmaSystemContext* InitializeContext() {
    msd_drv_ = msd::Driver::Create();
    if (!msd_drv_)
      return DRETP(nullptr, "failed to create msd driver");

    msd_drv_->Configure(MSD_DRIVER_CONFIG_TEST_NO_DEVICE_THREAD);

    auto msd_dev = msd_drv_->CreateDevice(GetTestDeviceHandle());
    if (!msd_dev)
      return DRETP(nullptr, "failed to create msd device");
    auto msd_connection_t = msd_dev->Open(0);
    if (!msd_connection_t)
      return DRETP(nullptr, "msd_device_open failed");
    system_dev_ = std::shared_ptr<msd::MagmaSystemDevice>(
        msd::MagmaSystemDevice::Create(msd_drv_.get(), std::move(msd_dev)));
    uint32_t ctx_id = 0;
    connection_ = std::unique_ptr<msd::MagmaSystemConnection>(
        new msd::MagmaSystemConnection(system_dev_, std::move(msd_connection_t)));
    if (!connection_)
      return DRETP(nullptr, "failed to connect to msd device");
    connection_->CreateContext(ctx_id);
    auto ctx = connection_->LookupContext(ctx_id);
    if (!ctx)
      return DRETP(nullptr, "failed to create context");
    return ctx;
  }

  std::unique_ptr<msd::Driver> msd_drv_;
  std::shared_ptr<msd::MagmaSystemDevice> system_dev_;
  std::unique_ptr<msd::MagmaSystemConnection> connection_;
};

TEST(CommandBuffer, TestInvalidSemaphoreImmediate) { ::Test().TestInvalidSemaphoreImmediate(); }
TEST(CommandBuffer, TestSemaphoreImmediate) { ::Test().TestSemaphoreImmediate(); }
TEST(CommandBuffer, TestValidImmediate) { ::Test().TestValidImmediate(); }
TEST(CommandBuffer, TestValidLarger) { ::Test().TestValidLarger(); }
TEST(CommandBuffer, TestInvalidTooLarge) { ::Test().TestInvalidTooLarge(); }
TEST(CommandBuffer, TestInvalidOverflow) { ::Test().TestInvalidOverflow(); }
TEST(CommandBuffer, TestInvalidZeroSize) { ::Test().TestInvalidZeroSize(); }
TEST(CommandBuffer, TestInvalidSmaller) { ::Test().TestInvalidSmaller(); }
TEST(CommandBuffer, TestInvalidInUse) { ::Test().TestInvalidInUse(); }
TEST(CommandBuffer, TestInvalidDependencyType) { ::Test().TestInvalidDependencyType(); }
TEST(CommandBuffer, TestInvalidDependencyNotSubmitted) {
  ::Test().TestInvalidDependencyNotSubmitted();
}
}  // namespace
