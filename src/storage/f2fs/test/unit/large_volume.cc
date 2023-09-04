// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <deque>
#include <map>
#include <vector>

#include <gtest/gtest.h>

#include "src/storage/f2fs/f2fs.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"
#include "unit_lib.h"

namespace f2fs {

using block_client::FakeBlockDevice;

namespace {

// |LargeFakeDevice| is a class that wraps |FakeBlockDevice| to test very large block device
// environments (here about 4TB). The physical size of the backup ramdisk for this class is 400MB,
// and any request that exceeds the physical size will result in an error.
constexpr uint64_t kRamDiskBlockCount = 819200ULL;
constexpr uint64_t kVirtualBlockCount = 8192000000ULL;
constexpr uint64_t kBlockSize = kDefaultSectorSize;
class LargeFakeDevice : public FakeBlockDevice {
 public:
  LargeFakeDevice() : FakeBlockDevice({kRamDiskBlockCount, kBlockSize, true}) {
    free_offsets.resize(kRamDiskBlockCount);
    std::iota(free_offsets.begin(), free_offsets.end(), 0);
  }

  // |LargeFakeDevice::FifoTransaction()| is thread unsafe. Do not use it in a multithreaded
  // environment.
  zx_status_t FifoTransaction(block_fifo_request_t* requests, size_t count) override {
    std::vector<block_fifo_request_t> new_request_list;
    for (size_t i = 0; i < count; ++i) {
      for (uint32_t j = 0; j < requests[i].length; ++j) {
        const uint64_t request_offset = requests[i].dev_offset + j;
        uint64_t ramdisk_offset;
        if (auto it = offset_mapping.find(request_offset); it == offset_mapping.end()) {
          if (requests[i].command.opcode == BLOCK_OPCODE_TRIM) {
            return ZX_OK;
          }

          if (free_offsets.empty()) {
            return ZX_ERR_NO_SPACE;
          }

          ramdisk_offset = free_offsets.front();
          free_offsets.pop_front();
          offset_mapping.insert(std::make_pair(request_offset, ramdisk_offset));
        } else {
          ramdisk_offset = it->second;
          if (requests[i].command.opcode == BLOCK_OPCODE_TRIM) {
            free_offsets.push_back(it->second);
            offset_mapping.erase(it);
          }
        }

        block_fifo_request_t new_request = requests[i];
        new_request.length = 1;
        new_request.vmo_offset += j;
        new_request.dev_offset = ramdisk_offset;
        new_request_list.push_back(new_request);
      }
    }
    return FakeBlockDevice::FifoTransaction(new_request_list.data(), new_request_list.size());
  }

  zx_status_t BlockGetInfo(fuchsia_hardware_block::wire::BlockInfo* out_info) const override {
    if (auto err = FakeBlockDevice::BlockGetInfo(out_info); err != ZX_OK) {
      return err;
    }

    out_info->block_count = kVirtualBlockCount;
    out_info->block_size = kBlockSize;

    return ZX_OK;
  }

 private:
  std::deque<uint64_t> free_offsets;
  std::map<uint64_t, uint64_t> offset_mapping;
};

void MkfsOnLargeFakeDev(std::unique_ptr<Bcache>* bc) {
  auto device = std::make_unique<LargeFakeDevice>();
  bool readonly_device = false;
  auto bc_or = CreateBcache(std::move(device), &readonly_device);
  ASSERT_TRUE(bc_or.is_ok());

  MkfsOptions options;
  MkfsWorker mkfs(std::move(*bc_or), options);
  auto ret = mkfs.DoMkfs();
  ASSERT_EQ(ret.is_error(), false);
  *bc = std::move(*ret);
}
}  // namespace

TEST(CpPayloadTest, ReadWrite) {
  std::unique_ptr<Bcache> bc;
  MkfsOnLargeFakeDev(&bc);

  // 1. Create f2fs and root dir
  std::unique_ptr<F2fs> fs;
  MountOptions options{};
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  fbl::RefPtr<VnodeF2fs> root;
  FileTester::CreateRoot(fs.get(), &root);
  fbl::RefPtr<Dir> root_dir;
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));

  fbl::RefPtr<fs::Vnode> test_file;
  ASSERT_EQ(root_dir->Create("test", S_IFREG, &test_file), ZX_OK);
  fbl::RefPtr<File> test_file_vn = fbl::RefPtr<File>::Downcast(std::move(test_file));

  // 2. Write random data
  char buf[kPageSize];
  char read_buf[kPageSize];
  for (size_t i = 0; i < kPageSize; ++i) {
    buf[i] = static_cast<char>(rand());
  }
  FileTester::AppendToFile(test_file_vn.get(), buf, kPageSize);

  // 3. Remount
  ASSERT_EQ(test_file_vn->Close(), ZX_OK);
  test_file_vn = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;
  fs->WriteCheckpoint(false, false);
  FileTester::Unmount(std::move(fs), &bc);
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);

  FileTester::CreateRoot(fs.get(), &root);
  root_dir = fbl::RefPtr<Dir>::Downcast(std::move(root));
  FileTester::Lookup(root_dir.get(), "test", &test_file);
  test_file_vn = fbl::RefPtr<File>::Downcast(std::move(test_file));

  // 4. Read and Verify
  FileTester::ReadFromFile(test_file_vn.get(), read_buf, kPageSize, 0);
  ASSERT_EQ(std::memcmp(read_buf, buf, kPageSize), 0);

  // 5. Make orphan
  FileTester::AppendToFile(test_file_vn.get(), buf, kPageSize);
  FileTester::DeleteChild(root_dir.get(), "test", false);
  fs->WriteCheckpoint(false, false);
  ASSERT_EQ(test_file_vn->Close(), ZX_OK);
  test_file_vn = nullptr;
  ASSERT_EQ(root_dir->Close(), ZX_OK);
  root_dir = nullptr;
  FileTester::SuddenPowerOff(std::move(fs), &bc);

  // 6. Remount
  FileTester::MountWithOptions(loop.dispatcher(), options, &bc, &fs);
  FileTester::Unmount(std::move(fs), &bc);

  // 7. Fsck
  ASSERT_EQ(Fsck(std::move(bc), FsckOptions{.repair = false}, &bc), ZX_OK);
}
}  // namespace f2fs
