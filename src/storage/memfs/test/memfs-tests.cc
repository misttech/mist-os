// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/zx/stream.h>
#include <lib/zx/vmo.h>
#include <sys/stat.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <limits>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"
#include "src/storage/memfs/memfs.h"
#include "src/storage/memfs/vnode_dir.h"

namespace memfs {
namespace {

constexpr uint64_t kMaxFileSize = std::numeric_limits<zx_off_t>::max() - (1ull << 17) + 1;

TEST(MemfsTest, DirectoryLifetime) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  zx::result result = memfs::Memfs::Create(loop.dispatcher(), "<tmp>");
  ASSERT_TRUE(result.is_ok()) << result.status_string();
}

TEST(MemfsTest, CreateFile) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  zx::result result = memfs::Memfs::Create(loop.dispatcher(), "<tmp>");
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  auto& [vfs, root] = result.value();
  fbl::RefPtr<fs::Vnode> file;
  ASSERT_OK(root->Create("foobar", S_IFREG, &file));
  auto directory = static_cast<fbl::RefPtr<fs::Vnode>>(root);
  fs::VnodeAttributes directory_attr, file_attr;
  ASSERT_OK(directory->GetAttributes(&directory_attr));
  ASSERT_OK(file->GetAttributes(&file_attr));

  // Directory created before file.
  ASSERT_LE(directory_attr.creation_time, file_attr.creation_time);

  // Observe that the modify time of the directory is larger than the file.
  // This implies "the file is created, then the directory is updated".
  ASSERT_GE(directory_attr.modification_time, file_attr.modification_time);
}

TEST(MemfsTest, SubdirectoryUpdateTime) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  zx::result result = memfs::Memfs::Create(loop.dispatcher(), "<tmp>");
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  auto& [vfs, root] = result.value();
  fbl::RefPtr<fs::Vnode> index;
  ASSERT_OK(root->Create("index", S_IFREG, &index));
  fbl::RefPtr<fs::Vnode> subdirectory;
  ASSERT_OK(root->Create("subdirectory", S_IFDIR, &subdirectory));

  // Write a file at "subdirectory/file".
  fbl::RefPtr<fs::Vnode> file;
  ASSERT_OK(subdirectory->Create("file", S_IFREG, &file));
  file->DidModifyStream();

  // Overwrite a file at "index".
  index->DidModifyStream();

  fs::VnodeAttributes subdirectory_attr, index_attr;
  ASSERT_OK(subdirectory->GetAttributes(&subdirectory_attr));
  ASSERT_OK(index->GetAttributes(&index_attr));

  // "index" was written after "subdirectory".
  ASSERT_LE(subdirectory_attr.modification_time, index_attr.modification_time);
}

TEST(MemfsTest, SubPageContentSize) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  zx::result result = memfs::Memfs::Create(loop.dispatcher(), "<tmp>");
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  auto& [vfs, root] = result.value();

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(zx_system_get_page_size(), 0, &vmo));

  // Set the content size to a non page aligned value.
  zx_off_t content_size = zx_system_get_page_size() / 2;
  EXPECT_OK(vmo.set_property(ZX_PROP_VMO_CONTENT_SIZE, &content_size, sizeof(content_size)));

  // Duplicate the handle to create the VMO file so we can use the original handle for testing.
  zx::vmo vmo_dup;
  ASSERT_OK(vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo_dup));
  // Create a VMO file sized to its content.
  ASSERT_OK(root->CreateFromVmo("vmo", vmo_dup.release(), 0, content_size));

  // Lookup the VMO and request its representation.
  fbl::RefPtr<fs::Vnode> vmo_vnode;
  ASSERT_OK(root->Lookup("vmo", &vmo_vnode));
  zx::vmo vnode_vmo;
  ASSERT_EQ(ZX_OK, vmo_vnode->GetVmo(fuchsia_io::wire::VmoFlags::kRead, &vnode_vmo));

  // We expect no cloning to have happened, this means we should have a handle to our original VMO.
  // We can verify this by comparing koids.
  zx_info_handle_basic_t original_vmo_info;
  ASSERT_OK(vmo.get_info(ZX_INFO_HANDLE_BASIC, &original_vmo_info, sizeof(original_vmo_info),
                         nullptr, nullptr));
  zx_info_handle_basic_t vnode_vmo_info;
  ASSERT_OK(vnode_vmo.get_info(ZX_INFO_HANDLE_BASIC, &vnode_vmo_info, sizeof(vnode_vmo_info),
                               nullptr, nullptr));
  EXPECT_EQ(original_vmo_info.koid, vnode_vmo_info.koid);
}

TEST(MemfsTest, LocalClone) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  zx::result result = memfs::Memfs::Create(loop.dispatcher(), "<tmp>");
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  auto& [vfs, root] = result.value();

  zx_off_t vmo_size = zx_system_get_page_size() * static_cast<zx_off_t>(2);
  zx_off_t vmo_offset = vmo_size / 2;

  // Offset is required to be page aligned and non-zero.
  ASSERT_EQ(vmo_offset % zx_system_get_page_size(), 0ull);
  ASSERT_NE(vmo_offset, 0ull);

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(vmo_size, 0, &vmo));

  zx_info_handle_basic_t original_vmo_info;
  ASSERT_OK(vmo.get_info(ZX_INFO_HANDLE_BASIC, &original_vmo_info, sizeof(original_vmo_info),
                         nullptr, nullptr));

  // Create a file from a VMO using a non-zero offset after which we should NOT get an exact copy.
  zx::vmo vmo_dup;
  fbl::RefPtr<fs::Vnode> vmo_vnode;
  zx_info_handle_basic_t vnode_vmo_info;

  ASSERT_OK(vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo_dup));
  ASSERT_OK(root->CreateFromVmo("vmo1", vmo_dup.release(), vmo_offset, vmo_size - vmo_offset));
  ASSERT_OK(root->Lookup("vmo1", &vmo_vnode));
  zx::vmo vnode_vmo;
  ASSERT_EQ(ZX_OK, vmo_vnode->GetVmo(fuchsia_io::wire::VmoFlags::kRead, &vnode_vmo));
  ASSERT_OK(vnode_vmo.get_info(ZX_INFO_HANDLE_BASIC, &vnode_vmo_info, sizeof(vnode_vmo_info),
                               nullptr, nullptr));
  EXPECT_NE(original_vmo_info.koid, vnode_vmo_info.koid);

  // Create a file from a VMO using a smaller size, after which we should NOT get an exact copy.
  ASSERT_OK(vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo_dup));
  ASSERT_OK(root->CreateFromVmo("vmo2", vmo_dup.release(), 0, vmo_size - 1));
  ASSERT_OK(root->Lookup("vmo2", &vmo_vnode));
  ASSERT_EQ(ZX_OK, vmo_vnode->GetVmo(fuchsia_io::wire::VmoFlags::kRead, &vnode_vmo));
  ASSERT_OK(vnode_vmo.get_info(ZX_INFO_HANDLE_BASIC, &vnode_vmo_info, sizeof(vnode_vmo_info),
                               nullptr, nullptr));
  EXPECT_NE(original_vmo_info.koid, vnode_vmo_info.koid);
}

TEST(MemfsTest, TruncateZerosTail) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  zx::result result = memfs::Memfs::Create(loop.dispatcher(), "<tmp>");
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  auto& [vfs, root] = result.value();

  fbl::RefPtr<fs::Vnode> file;
  ASSERT_OK(root->Create("file", S_IFREG, &file));

  zx::stream stream;
  ASSERT_OK(file->CreateStream(ZX_STREAM_MODE_READ | ZX_STREAM_MODE_WRITE, &stream));

  std::string data = "file-contents";
  zx_iovec_t iov = {
      .buffer = data.data(),
      .capacity = data.size(),
  };
  size_t actual = 0;
  ASSERT_OK(stream.writev_at(0, 500, &iov, 1, &actual));
  ASSERT_EQ(actual, data.size());

  // Shrink the file to before the write.
  file->Truncate(300);
  // Grow the file back to after the write.
  file->Truncate(600);

  // Verify the data is gone.
  std::vector<char> file_contents(data.size(), 0);
  iov.buffer = file_contents.data();
  ASSERT_OK(stream.readv_at(0, 500, &iov, 1, &actual));
  ASSERT_EQ(actual, data.size());
  EXPECT_THAT(file_contents, testing::Each('\0'));
}

TEST(MemfsTest, WriteMaxFileSize) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  zx::result result = memfs::Memfs::Create(loop.dispatcher(), "<tmp>");
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  auto& [vfs, root] = result.value();

  fbl::RefPtr<fs::Vnode> file;
  ASSERT_OK(root->Create("file", S_IFREG, &file));

  zx::stream stream;
  ASSERT_OK(file->CreateStream(ZX_STREAM_MODE_READ | ZX_STREAM_MODE_WRITE, &stream));

  std::string data = "1";
  zx_iovec_t iov = {
      .buffer = data.data(),
      .capacity = data.size(),
  };
  size_t actual = 0;
  ASSERT_OK(stream.writev_at(0, kMaxFileSize - 1, &iov, 1, &actual));
  ASSERT_EQ(actual, data.size());
  fs::VnodeAttributes attributes;
  ASSERT_OK(file->GetAttributes(&attributes));
  ASSERT_EQ(attributes.content_size, kMaxFileSize);

  // Try to write beyond the max file size.
  ASSERT_STATUS(stream.writev_at(0, kMaxFileSize, &iov, 1, &actual), ZX_ERR_OUT_OF_RANGE);
}

TEST(MemfsTest, TruncateToMaxFileSize) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  zx::result result = memfs::Memfs::Create(loop.dispatcher(), "<tmp>");
  ASSERT_TRUE(result.is_ok()) << result.status_string();
  auto& [vfs, root] = result.value();

  fbl::RefPtr<fs::Vnode> file;
  ASSERT_OK(root->Create("file", S_IFREG, &file));

  ASSERT_OK(file->Truncate(kMaxFileSize));
  fs::VnodeAttributes attributes;
  ASSERT_OK(file->GetAttributes(&attributes));
  ASSERT_EQ(attributes.content_size, kMaxFileSize);

  // Try to truncate beyond the max file size.
  ASSERT_STATUS(file->Truncate(kMaxFileSize + 1), ZX_ERR_OUT_OF_RANGE);
}

}  // namespace
}  // namespace memfs
