// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/zx/resource.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#include <cstddef>
#include <memory>
#include <optional>

#include <fuzzer/FuzzedDataProvider.h>

#include "fbl/unique_fd.h"
#include "fidl/fuchsia.io/cpp/markers.h"
#include "lib/fdio/fd.h"
#include "lib/fidl/cpp/wire/channel.h"
#include "lib/fidl/cpp/wire/internal/transport_channel.h"
#include "src/storage/blobfs/common.h"
#include "src/storage/blobfs/component_runner.h"
#include "src/storage/blobfs/delivery_blob.h"
#include "src/storage/blobfs/mkfs.h"
#include "src/storage/blobfs/mount.h"
#include "src/storage/blobfs/test/blob_utils.h"
#include "src/storage/blobfs/test/unit/local_decompressor_creator.h"
#include "src/storage/lib/block_client/cpp/fake_block_device.h"

namespace blobfs {
namespace {

constexpr uint32_t kBlockDeviceSize = 128 * 1024 * 1024;
constexpr uint32_t kMaxBlobSize = 96 * 1024 * 1024;
constexpr uint32_t kBlockSize = 512;
constexpr uint32_t kNumBlocks = kBlockDeviceSize / kBlockSize;

fidl::ClientEnd<fuchsia_io::Directory> ServeOutgoingDirectory(ComponentRunner& runner) {
  auto root_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
  auto status =
      runner.ServeRoot(std::move(root_endpoints.server),
                       fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle>(), zx::resource());
  ZX_ASSERT(status.is_ok());
  return std::move(root_endpoints.client);
}

fidl::ClientEnd<fuchsia_io::Directory> GetRootDirectory(
    fidl::ClientEnd<fuchsia_io::Directory>& outgoing) {
  auto root_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
  auto status = fidl::WireCall(outgoing)->Open(
      fuchsia_io::wire::OpenFlags::kRightReadable | fuchsia_io::wire::OpenFlags::kRightWritable |
          fuchsia_io::wire::OpenFlags::kDirectory,
      {}, "root", fidl::ServerEnd<fuchsia_io::Node>(root_endpoints.server.TakeChannel()));
  ZX_ASSERT(status.ok());
  return std::move(root_endpoints.client);
}

class BlobfsInstance {
 public:
  explicit BlobfsInstance()
      : loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
        runner_(loop_, ComponentOptions{.pager_threads = 1}),
        local_decompressor_creator_(LocalDecompressorCreator::Create().value()) {
    loop_.StartThread();
    auto device = std::make_unique<block_client::FakeBlockDevice>(kNumBlocks, kBlockSize);
    ZX_ASSERT(FormatFilesystem(device.get(), FilesystemOptions{}) == ZX_OK);
    auto outgoing = ServeOutgoingDirectory(runner_);
    ZX_ASSERT(runner_
                  .Configure(std::move(device),
                             MountOptions{
                                 .cache_policy = CachePolicy::EvictImmediately,
                                 .decompression_connector =
                                     &local_decompressor_creator_->GetDecompressorConnector(),
                                 .paging_threads = 1,
                             })
                  .is_ok());
    auto root = GetRootDirectory(outgoing);
    ZX_ASSERT(fdio_fd_create(root.TakeChannel().release(), root_fd_.reset_and_get_address()) ==
              ZX_OK);
    ZX_ASSERT(root_fd_.is_valid());
  }

  const fbl::unique_fd& root_fd() const { return root_fd_; }

 private:
  async::Loop loop_;
  ComponentRunner runner_;
  std::unique_ptr<LocalDecompressorCreator> local_decompressor_creator_;
  fbl::unique_fd root_fd_;
};

std::optional<bool> GetDeliveryBlobCompression(FuzzedDataProvider& provider) {
  enum class DeliveryBlobCompression : uint8_t {
    kAlwaysCompress,
    kNeverCompress,
    kMaybeCompress,
    kMaxValue = kMaybeCompress,
  };
  switch (provider.ConsumeEnum<DeliveryBlobCompression>()) {
    case DeliveryBlobCompression::kAlwaysCompress:
      return true;
    case DeliveryBlobCompression::kNeverCompress:
      return false;
    case DeliveryBlobCompression::kMaybeCompress:
      return std::nullopt;
  }
}

void WriteBlob(const fbl::unique_fd& root_fd, const BlobInfo& blob_info,
               std::optional<bool> compress) {
  auto delivery_blob =
      GenerateDeliveryBlobType1({blob_info.data.get(), blob_info.size_data}, compress);
  ZX_ASSERT(delivery_blob.is_ok());
  auto delivery_blob_name = GetDeliveryBlobPath(blob_info.GetMerkleRoot());

  fbl::unique_fd blob_fd(
      openat(root_fd.get(), delivery_blob_name.c_str(), O_CREAT | O_WRONLY, S_IRUSR | S_IWUSR));
  ZX_ASSERT(blob_fd.is_valid());
  ZX_ASSERT_MSG(ftruncate(blob_fd.get(), delivery_blob->size()) == 0, "%s", strerror(errno));
  ZX_ASSERT_MSG(StreamAll(write, blob_fd.get(), delivery_blob->data(), delivery_blob->size()) == 0,
                "%s", strerror(errno));
}

void PageInBlob(const fbl::unique_fd& root_fd, const BlobInfo& blob_info) {
  fbl::unique_fd blob_fd(openat(root_fd.get(), blob_info.path, O_RDONLY, S_IRUSR | S_IWUSR));
  ZX_ASSERT(blob_fd.is_valid());
  void* addr = mmap(nullptr, blob_info.size_data, PROT_READ, MAP_PRIVATE, blob_fd.get(), 0);
  ZX_ASSERT(addr != MAP_FAILED);
  ZX_ASSERT(memcmp(addr, blob_info.data.get(), blob_info.size_data) == 0);
}

void RemoveBlob(const fbl::unique_fd& root_fd, const BlobInfo& blob_info) {
  ZX_ASSERT(unlinkat(root_fd.get(), blob_info.path, 0) == 0);
}

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
  static const BlobfsInstance* blobfs = new BlobfsInstance();
  FuzzedDataProvider provider(data, size);
  size_t blob_size = provider.ConsumeIntegralInRange<size_t>(1, kMaxBlobSize);
  auto blob_info = GenerateRealisticBlob("", blob_size);

  WriteBlob(blobfs->root_fd(), *blob_info, GetDeliveryBlobCompression(provider));
  // The contents of a newly created blob is not cached and will be paged back in.
  PageInBlob(blobfs->root_fd(), *blob_info);
  RemoveBlob(blobfs->root_fd(), *blob_info);
  return 0;
}

}  // namespace
}  // namespace blobfs
