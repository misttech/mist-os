// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/fs_test/fs_test.h"

#include <fcntl.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fuchsia/hardware/block/driver/c/banjo.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <unistd.h>
#include <zircon/errors.h>

#include <cstddef>
#include <cstring>
#include <memory>
#include <string>
#include <utility>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <storage/buffer/owned_vmoid.h>

#include "src/devices/block/drivers/core/block-fifo.h"
#include "src/storage/fs_test/fs_test_fixture.h"
#include "src/storage/lib/block_client/cpp/remote_block_device.h"
#include "src/storage/lib/fs_management/cpp/format.h"

namespace fs_test {
namespace {

using DeviceTest = fs_test::FilesystemTest;

// Defaults to a filesize of 1 megabyte.
void CreateFxFile(const std::string& file_name, const off_t file_size = 1024 * 1024) {
  fbl::unique_fd fd(open(file_name.c_str(), O_CREAT | O_RDWR, 0666));
  ASSERT_TRUE(fd);
  ASSERT_EQ(ftruncate(fd.get(), file_size), 0);
}

TEST_P(DeviceTest, TestValidDiskFormat) {
  ASSERT_TRUE(fs().Unmount().is_ok());
  zx::result path = fs().DevicePath();
  ASSERT_TRUE(path.is_ok());
  zx::result channel = component::Connect<fuchsia_hardware_block::Block>(path.value());
  ASSERT_TRUE(channel.is_ok());
  ASSERT_EQ(fs_management::DetectDiskFormat(channel.value()), fs_management::kDiskFormatFxfs);
}

TEST_P(DeviceTest, TestWriteThenRead) {
  const std::string kFilename = GetPath("block_device");
  const off_t kFileSize = 10 * 1024 * 1024;  // 10 megabytes
  CreateFxFile(kFilename, kFileSize);
  auto [client, server] = fidl::Endpoints<fuchsia_hardware_block_volume::Volume>::Create();

  // Re-open file as block device i.e. using OpenFlag.BLOCK_DEVICE.
  fdio_cpp::FdioCaller caller(fs().GetRootFd());
  // TODO(https://fxbug.dev/378924259): Migrate to new Open signature.
  ASSERT_EQ(fidl::WireCall(caller.directory())
                ->DeprecatedOpen(fuchsia_io::wire::OpenFlags::kRightReadable |
                                     fuchsia_io::wire::OpenFlags::kRightWritable |
                                     fuchsia_io::wire::OpenFlags::kBlockDevice,
                                 {}, "block_device",
                                 fidl::ServerEnd<fuchsia_io::Node>(server.TakeChannel()))
                .status(),
            ZX_OK);

  zx::result result = block_client::RemoteBlockDevice::Create(std::move(client));
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  std::unique_ptr<block_client::RemoteBlockDevice> device = std::move(result.value());

  fuchsia_hardware_block::wire::BlockInfo info = {};
  ASSERT_EQ(device->BlockGetInfo(&info), ZX_OK);
  ASSERT_EQ(info.block_count, static_cast<unsigned long>(kFileSize) / info.block_size);

  zx::vmo vmo;
  storage::OwnedVmoid vmoid;
  const size_t kVmoBlocks = 5;
  ASSERT_EQ(zx::vmo::create(kVmoBlocks * info.block_size, 0, &vmo), ZX_OK);
  ASSERT_NO_FATAL_FAILURE(device->BlockAttachVmo(vmo, &vmoid.GetReference(device.get())));

  const size_t kVmoWriteBlocks = 2;
  const size_t kVmoBlockOffset = 1;
  ASSERT_LE(kVmoBlockOffset + kVmoWriteBlocks, kVmoBlocks);
  const size_t kBufLen = kVmoWriteBlocks * info.block_size;
  auto write_buf = std::make_unique<char[]>(kBufLen);
  memset(write_buf.get(), 0xa3, kBufLen);
  ASSERT_EQ(vmo.write(write_buf.get(), kVmoBlockOffset * info.block_size, kBufLen), ZX_OK);

  block_fifo_request_t write_request;
  write_request.command = {.opcode = BLOCK_OPCODE_WRITE, .flags = 0};
  write_request.vmoid = vmoid.get();
  write_request.length = kVmoWriteBlocks;
  write_request.vmo_offset = kVmoBlockOffset;
  write_request.dev_offset = 0;
  EXPECT_EQ(device->FifoTransaction(&write_request, 1), ZX_OK);

  // "Clear" vmo, so any data in the vmo after is solely dependent on the following
  // BLOCK_OPCODE_READ
  const size_t kZeroBufLen = kVmoBlocks * info.block_size;
  auto zero_buf = std::make_unique<char[]>(kZeroBufLen);
  memset(zero_buf.get(), 0, kZeroBufLen);
  ASSERT_EQ(vmo.write(zero_buf.get(), 0, kZeroBufLen), ZX_OK);

  auto read_buf = std::make_unique<char[]>(kBufLen);
  memset(read_buf.get(), 0, kBufLen);
  block_fifo_request_t read_request;
  read_request.command = {.opcode = BLOCK_OPCODE_READ, .flags = 0};
  read_request.vmoid = vmoid.get();
  read_request.length = kVmoWriteBlocks;
  read_request.vmo_offset = kVmoBlockOffset;
  read_request.dev_offset = 0;
  ASSERT_EQ(device->FifoTransaction(&read_request, 1), ZX_OK);
  ASSERT_EQ(vmo.read(read_buf.get(), kVmoBlockOffset * info.block_size, kBufLen), ZX_OK);
  EXPECT_EQ(memcmp(write_buf.get(), read_buf.get(), kBufLen), 0);
}

// Tests multiple reads and writes in a group
TEST_P(DeviceTest, TestGroupWritesThenReads) {
  const std::string kFilename = GetPath("block_device");
  CreateFxFile(kFilename);
  auto [client, server] = fidl::Endpoints<fuchsia_hardware_block_volume::Volume>::Create();

  fdio_cpp::FdioCaller caller(fs().GetRootFd());
  // TODO(https://fxbug.dev/378924259): Migrate to new Open signature.
  ASSERT_EQ(fidl::WireCall(caller.directory())
                ->DeprecatedOpen(fuchsia_io::wire::OpenFlags::kRightReadable |
                                     fuchsia_io::wire::OpenFlags::kRightWritable |
                                     fuchsia_io::wire::OpenFlags::kBlockDevice,
                                 {}, "block_device",
                                 fidl::ServerEnd<fuchsia_io::Node>(server.TakeChannel()))
                .status(),
            ZX_OK);

  zx::result result = block_client::RemoteBlockDevice::Create(std::move(client));
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  std::unique_ptr<block_client::RemoteBlockDevice> device = std::move(result.value());

  const size_t kVmoBlocks = 6;
  zx::vmo vmo;
  storage::OwnedVmoid vmoid;
  fuchsia_hardware_block::wire::BlockInfo info = {};
  ASSERT_EQ(device->BlockGetInfo(&info), ZX_OK);
  ASSERT_EQ(zx::vmo::create(kVmoBlocks * info.block_size, 0, &vmo), ZX_OK);
  ASSERT_NO_FATAL_FAILURE(device->BlockAttachVmo(vmo, &vmoid.GetReference(device.get())));

  // The first group of writes will send 2 write requests, with a buffer size of kVmoWriteBlocks *
  // block_size This test will write and read from vmo and device with an offset to test that read
  // and writes work with offsets
  const size_t kVmoWriteBlocks = 2;
  const size_t kOffsetBlocks = 1;
  ASSERT_LE(kOffsetBlocks + 2 * kVmoWriteBlocks, kVmoBlocks);

  // Write write_buf1 and write_buf2 to vmo with offset = kOffsetBlocks
  const size_t kWriteBufLen = kVmoWriteBlocks * info.block_size;
  auto write_buf1 = std::make_unique<char[]>(kWriteBufLen);
  memset(write_buf1.get(), 0xa3, kWriteBufLen);
  ASSERT_EQ(vmo.write(write_buf1.get(), kOffsetBlocks * info.block_size, kWriteBufLen), ZX_OK);

  auto write_buf2 = std::make_unique<char[]>(kWriteBufLen);
  memset(write_buf2.get(), 0xf7, kWriteBufLen);
  ASSERT_EQ(vmo.write(write_buf2.get(), (kOffsetBlocks + kVmoWriteBlocks) * info.block_size,
                      kWriteBufLen),
            ZX_OK);

  block_fifo_request_t write_requests[2];
  write_requests[0].command = {.opcode = BLOCK_OPCODE_WRITE, .flags = 0};
  write_requests[0].vmoid = vmoid.get();
  write_requests[0].length = kVmoWriteBlocks;
  write_requests[0].vmo_offset = kOffsetBlocks;
  write_requests[0].dev_offset = 0;

  write_requests[1].command = {.opcode = BLOCK_OPCODE_WRITE, .flags = 0};
  write_requests[1].vmoid = vmoid.get();
  write_requests[1].length = kVmoWriteBlocks;
  write_requests[1].vmo_offset = kOffsetBlocks + kVmoWriteBlocks;
  write_requests[1].dev_offset = kVmoWriteBlocks;
  EXPECT_EQ(device->FifoTransaction(write_requests, std::size(write_requests)), ZX_OK);

  const size_t kReadBufLen = kVmoBlocks * info.block_size;
  auto read_buf = std::make_unique<char[]>(kReadBufLen);
  memset(read_buf.get(), 0, kReadBufLen);
  ASSERT_EQ(vmo.write(read_buf.get(), 0, kReadBufLen), ZX_OK);

  block_fifo_request_t read_requests[2];
  read_requests[0].command = {.opcode = BLOCK_OPCODE_READ, .flags = 0};
  read_requests[0].vmoid = vmoid.get();
  read_requests[0].length = kVmoWriteBlocks;
  read_requests[0].vmo_offset = kOffsetBlocks;
  read_requests[0].dev_offset = 0;

  read_requests[1].command = {.opcode = BLOCK_OPCODE_READ, .flags = 0};
  read_requests[1].vmoid = vmoid.get();
  read_requests[1].length = kVmoWriteBlocks;
  read_requests[1].vmo_offset = kOffsetBlocks + kVmoWriteBlocks;
  read_requests[1].dev_offset = kVmoWriteBlocks;
  ASSERT_EQ(device->FifoTransaction(read_requests, std::size(read_requests)), ZX_OK);
  ASSERT_EQ(vmo.read(read_buf.get(), 0, kReadBufLen), ZX_OK);
  EXPECT_EQ(
      memcmp(write_buf1.get(), read_buf.get() + (kOffsetBlocks * info.block_size), kWriteBufLen),
      0);
  EXPECT_EQ(
      memcmp(write_buf2.get(),
             read_buf.get() + ((kOffsetBlocks + kVmoWriteBlocks) * info.block_size), kWriteBufLen),
      0);
}

TEST_P(DeviceTest, TestWriteThenFlushThenRead) {
  const std::string kFilename = GetPath("block_device");
  CreateFxFile(kFilename);
  auto [client, server] = fidl::Endpoints<fuchsia_hardware_block_volume::Volume>::Create();

  fdio_cpp::FdioCaller caller(fs().GetRootFd());
  // TODO(https://fxbug.dev/378924259): Migrate to new Open signature.
  ASSERT_EQ(fidl::WireCall(caller.directory())
                ->DeprecatedOpen(fuchsia_io::wire::OpenFlags::kRightReadable |
                                     fuchsia_io::wire::OpenFlags::kRightWritable |
                                     fuchsia_io::wire::OpenFlags::kBlockDevice,
                                 {}, "block_device",
                                 fidl::ServerEnd<fuchsia_io::Node>(server.TakeChannel()))
                .status(),
            ZX_OK);

  zx::result result = block_client::RemoteBlockDevice::Create(std::move(client));
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  std::unique_ptr<block_client::RemoteBlockDevice> device = std::move(result.value());

  const size_t kVmoBlocks = 2;
  zx::vmo vmo;
  storage::OwnedVmoid vmoid;
  fuchsia_hardware_block::wire::BlockInfo info = {};
  ASSERT_EQ(device->BlockGetInfo(&info), ZX_OK);
  ASSERT_EQ(zx::vmo::create(kVmoBlocks * info.block_size, 0, &vmo), ZX_OK);
  ASSERT_NO_FATAL_FAILURE(device->BlockAttachVmo(vmo, &vmoid.GetReference(device.get())));

  const size_t kBufLen = kVmoBlocks * info.block_size;
  auto write_buf = std::make_unique<char[]>(kBufLen);
  memset(write_buf.get(), 0xa3, kBufLen);
  ASSERT_EQ(vmo.write(write_buf.get(), 0, kBufLen), ZX_OK);

  block_fifo_request_t requests[2];
  requests[0].command = {.opcode = BLOCK_OPCODE_WRITE, .flags = 0};
  requests[0].vmoid = vmoid.get();
  requests[0].length = kVmoBlocks;
  requests[0].vmo_offset = 0;
  requests[0].dev_offset = 0;

  requests[1].command = {.opcode = BLOCK_OPCODE_FLUSH, .flags = 0};
  requests[1].vmoid = vmoid.get();
  requests[1].length = 0;
  requests[1].vmo_offset = 0;
  requests[1].dev_offset = 0;
  EXPECT_EQ(device->FifoTransaction(requests, std::size(requests)), ZX_OK);

  auto read_buf = std::make_unique<char[]>(kBufLen);
  memset(read_buf.get(), 0, kBufLen);
  ASSERT_EQ(vmo.write(read_buf.get(), 0, kBufLen), ZX_OK);

  block_fifo_request_t read_request;
  read_request.command = {.opcode = BLOCK_OPCODE_READ, .flags = 0};
  read_request.vmoid = vmoid.get();
  read_request.length = kVmoBlocks;
  read_request.vmo_offset = 0;
  read_request.dev_offset = 0;
  ASSERT_EQ(device->FifoTransaction(&read_request, 1), ZX_OK);
  ASSERT_EQ(vmo.read(read_buf.get(), 0, kBufLen), ZX_OK);
  EXPECT_EQ(memcmp(write_buf.get(), read_buf.get(), kBufLen), 0);
}

TEST_P(DeviceTest, TestInvalidGroupRequests) {
  const std::string kFilename = GetPath("block_device");
  CreateFxFile(kFilename);
  auto [client, server] = fidl::Endpoints<fuchsia_hardware_block_volume::Volume>::Create();

  fdio_cpp::FdioCaller caller(fs().GetRootFd());
  // TODO(https://fxbug.dev/378924259): Migrate to new Open signature.
  ASSERT_EQ(fidl::WireCall(caller.directory())
                ->DeprecatedOpen(fuchsia_io::wire::OpenFlags::kRightReadable |
                                     fuchsia_io::wire::OpenFlags::kRightWritable |
                                     fuchsia_io::wire::OpenFlags::kBlockDevice,
                                 {}, "block_device",
                                 fidl::ServerEnd<fuchsia_io::Node>(server.TakeChannel()))
                .status(),
            ZX_OK);

  zx::result result = block_client::RemoteBlockDevice::Create(std::move(client));
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  std::unique_ptr<block_client::RemoteBlockDevice> device = std::move(result.value());

  const size_t kVmoBlocks = 5;
  zx::vmo vmo;
  storage::OwnedVmoid vmoid;
  fuchsia_hardware_block::wire::BlockInfo info = {};
  ASSERT_EQ(device->BlockGetInfo(&info), ZX_OK);
  ASSERT_EQ(zx::vmo::create(kVmoBlocks * info.block_size, 0, &vmo), ZX_OK);
  ASSERT_NO_FATAL_FAILURE(device->BlockAttachVmo(vmo, &vmoid.GetReference(device.get())));

  block_fifo_request_t requests[3];
  requests[0].command = {.opcode = BLOCK_OPCODE_FLUSH, .flags = 0};
  requests[0].vmoid = vmoid.get();
  requests[0].length = 0;
  requests[0].vmo_offset = 0;
  requests[0].dev_offset = 0;

  // Not a valid request
  requests[1].command = {.opcode = BLOCK_OPCODE_CLOSE_VMO, .flags = 0};
  requests[1].vmoid = 100;
  requests[1].length = 0;
  requests[1].vmo_offset = 0;
  requests[1].dev_offset = 0;

  requests[2].command = {.opcode = BLOCK_OPCODE_FLUSH, .flags = 0};
  requests[2].vmoid = vmoid.get();
  requests[2].length = 0;
  requests[2].vmo_offset = 0;
  requests[2].dev_offset = 0;
  EXPECT_NE(device->FifoTransaction(requests, std::size(requests)), ZX_OK);
}

INSTANTIATE_TEST_SUITE_P(/*no prefix*/, DeviceTest, testing::ValuesIn(AllTestFilesystems()),
                         testing::PrintToStringParamName());
}  // namespace
}  // namespace fs_test
