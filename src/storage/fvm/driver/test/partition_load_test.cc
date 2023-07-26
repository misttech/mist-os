// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.partition/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <lib/driver-integration-test/fixture.h>
#include <lib/fdio/cpp/caller.h>
#include <sys/types.h>

#include <cstdint>
#include <memory>
#include <utility>

#include <fbl/string_buffer.h>
#include <zxtest/zxtest.h>

#include "src/storage/fvm/format.h"
#include "src/storage/fvm/test_support.h"

namespace fvm {
namespace {

constexpr uint64_t kBlockSize = 512;
constexpr uint64_t kSliceSize = 1 << 20;

using driver_integration_test::IsolatedDevmgr;

class FvmVPartitionLoadTest : public zxtest::Test {
 public:
  static void SetUpTestSuite() {
    IsolatedDevmgr::Args args;
    args.disable_block_watcher = true;

    devmgr_ = std::make_unique<IsolatedDevmgr>();
    ASSERT_OK(IsolatedDevmgr::Create(&args, devmgr_.get()));
  }

  static void TearDownTestSuite() { devmgr_.reset(); }

 protected:
  static std::unique_ptr<IsolatedDevmgr> devmgr_;
};

std::unique_ptr<IsolatedDevmgr> FvmVPartitionLoadTest::devmgr_ = nullptr;

TEST_F(FvmVPartitionLoadTest, LoadPartitionWithPlaceHolderGuidIsUpdated) {
  constexpr uint64_t kBlockCount = (50 * kSliceSize) / kBlockSize;

  std::unique_ptr<RamdiskRef> ramdisk =
      RamdiskRef::Create(devmgr_->devfs_root(), kBlockSize, kBlockCount);
  ASSERT_TRUE(ramdisk);

  std::unique_ptr<FvmAdapter> fvm =
      FvmAdapter::Create(devmgr_->devfs_root(), kBlockSize, kBlockCount, kSliceSize, ramdisk.get());
  ASSERT_TRUE(fvm);

  std::unique_ptr<VPartitionAdapter> vpartition = nullptr;
  ASSERT_OK(fvm->AddPartition(devmgr_->devfs_root(), "test-partition",
                              static_cast<fvm::Guid>(fvm::kPlaceHolderInstanceGuid.data()),
                              static_cast<fvm::Guid>(fvm::kPlaceHolderInstanceGuid.data()), 1,
                              &vpartition));

  // Save the topological path for rebinding.  The topological path will be
  // consistent after rebinding the ramdisk, whereas the
  // /dev/class/block/[NNN] will issue a new number.
  zx::result controller = vpartition->GetController();
  ASSERT_OK(controller);
  const fidl::WireResult result = fidl::WireCall(controller.value())->GetTopologicalPath();
  ASSERT_OK(result.status());
  const fit::result response = result.value();
  ASSERT_TRUE(response.is_ok(), "%s", zx_status_get_string(response.error_value()));
  std::string_view topological_path = response.value()->path.get();
  // Strip off the leading /dev/; because we use an isolated devmgr, we need
  // relative paths, but ControllerGetTopologicalPath returns an absolute path
  // with the assumption that devfs is rooted at /dev.
  constexpr std::string_view kHeader = "/dev/";
  ASSERT_TRUE(cpp20::starts_with(topological_path, kHeader));
  std::string partition_path(topological_path.substr(kHeader.size()));

  {
    // After rebind the instance guid should not be kPlaceHolderGUID.
    ASSERT_OK(fvm->Rebind({}));

    zx::result channel =
        device_watcher::RecursiveWaitForFile(devmgr_->devfs_root().get(), partition_path.c_str());
    ASSERT_OK(channel.status_value());

    fidl::ClientEnd<fuchsia_hardware_block_partition::Partition> client_end(
        std::move(channel.value()));
    auto result = fidl::WireCall(client_end)->GetInstanceGuid();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    fidl::Array fidl = result.value().guid.get()->value;
    decltype(fidl)::value_type bytes[decltype(fidl)::size()];
    std::copy(fidl.begin(), fidl.end(), std::begin(bytes));
    Guid guid(bytes);
    EXPECT_NE(vpartition->guid(), guid);
    vpartition->guid() = guid;
  }
  {
    // One more time to check that the UUID persisted, so it doesn't change between 'reboot'.
    ASSERT_OK(fvm->Rebind({}));

    zx::result channel =
        device_watcher::RecursiveWaitForFile(devmgr_->devfs_root().get(), partition_path.c_str());
    ASSERT_OK(channel.status_value());

    fidl::ClientEnd<fuchsia_hardware_block_partition::Partition> client_end(
        std::move(channel.value()));
    auto result = fidl::WireCall(client_end)->GetInstanceGuid();
    ASSERT_OK(result.status());
    ASSERT_OK(result.value().status);
    fidl::Array fidl = result.value().guid.get()->value;
    decltype(fidl)::value_type bytes[decltype(fidl)::size()];
    std::copy(fidl.begin(), fidl.end(), std::begin(bytes));
    Guid guid(bytes);
    EXPECT_EQ(vpartition->guid(), guid);
  }
}

}  // namespace
}  // namespace fvm
