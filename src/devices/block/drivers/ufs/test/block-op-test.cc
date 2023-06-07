// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "unit-lib.h"

namespace ufs {
using BlockOpTest = UfsTest;

TEST_F(BlockOpTest, ReadTest) {
  const uint8_t kTestLun = 0;
  ASSERT_NO_FATAL_FAILURE(RunInit());
  while (device_->child_count() == 0) {
    zx::nanosleep(zx::deadline_after(zx::msec(1)));
  }

  zx_device* lu_dev = device_->GetLatestChild();

  char buf[ufs_mock_device::kMockBlockSize];
  std::strcpy(buf, "test");
  ASSERT_OK(mock_device_->BufferWrite(kTestLun, buf, 1, 0));

  ddk::BlockImplProtocolClient client(lu_dev);
  block_info_t info;
  uint64_t op_size;
  client.Query(&info, &op_size);

  sync_completion_t done;
  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(ufs_mock_device::kMockBlockSize, 0, &vmo));
  auto block_op = std::make_unique<uint8_t[]>(op_size);
  auto op = reinterpret_cast<block_op_t*>(block_op.get());
  *op = {
      .rw =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_READ,
                  },
              .vmo = vmo.get(),
              .length = 1,
              .offset_dev = 0,
              .offset_vmo = 0,
          },
  };
  client.Queue(op, callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);

  zx_vaddr_t vaddr;
  ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ, 0, vmo, 0, ufs_mock_device::kMockBlockSize,
                                       &vaddr));
  char* mapped_vaddr = reinterpret_cast<char*>(vaddr);
  ASSERT_EQ(std::memcmp(buf, mapped_vaddr, ufs_mock_device::kMockBlockSize), 0);
  ASSERT_OK(zx::vmar::root_self()->unmap(vaddr, ufs_mock_device::kMockBlockSize));
}

TEST_F(BlockOpTest, WriteTest) {
  const uint8_t kTestLun = 0;
  ASSERT_NO_FATAL_FAILURE(RunInit());
  while (device_->child_count() == 0) {
    zx::nanosleep(zx::deadline_after(zx::msec(1)));
  }

  zx_device* lu_dev = device_->GetLatestChild();

  ddk::BlockImplProtocolClient client(lu_dev);
  block_info_t info;
  uint64_t op_size;
  client.Query(&info, &op_size);

  sync_completion_t done;
  auto callback = [](void* ctx, zx_status_t status, block_op_t* op) {
    EXPECT_OK(status);
    sync_completion_signal(static_cast<sync_completion_t*>(ctx));
  };

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(ufs_mock_device::kMockBlockSize, 0, &vmo));

  zx_vaddr_t vaddr;
  ASSERT_OK(zx::vmar::root_self()->map(ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo, 0,
                                       ufs_mock_device::kMockBlockSize, &vaddr));
  char* mapped_vaddr = reinterpret_cast<char*>(vaddr);
  std::strcpy(mapped_vaddr, "test");

  auto block_op = std::make_unique<uint8_t[]>(op_size);
  auto op = reinterpret_cast<block_op_t*>(block_op.get());
  *op = {
      .rw =
          {
              .command =
                  {
                      .opcode = BLOCK_OPCODE_WRITE,
                  },
              .vmo = vmo.get(),
              .length = 1,
              .offset_dev = 0,
              .offset_vmo = 0,
          },
  };
  client.Queue(op, callback, &done);
  sync_completion_wait(&done, ZX_TIME_INFINITE);

  char buf[ufs_mock_device::kMockBlockSize];
  ASSERT_OK(mock_device_->BufferRead(kTestLun, buf, 1, 0));

  ASSERT_EQ(std::memcmp(buf, mapped_vaddr, ufs_mock_device::kMockBlockSize), 0);
  ASSERT_OK(zx::vmar::root_self()->unmap(vaddr, ufs_mock_device::kMockBlockSize));
}

}  // namespace ufs
