// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/test/integration/fdio_test.h"

#include <fidl/fuchsia.fs/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async/cpp/executor.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/diagnostics/reader/cpp/archive_reader.h>
#include <lib/diagnostics/reader/cpp/inspect.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>

#include "lib/fidl/cpp/wire/channel.h"
#include "lib/fidl/cpp/wire/internal/transport_channel.h"
#include "src/storage/blobfs/mkfs.h"
#include "src/storage/lib/fs_management/cpp/admin.h"
#include "src/storage/lib/fs_management/cpp/mount.h"

namespace blobfs {

constexpr uint64_t kBlockSize = 512;
constexpr uint64_t kNumBlocks = 8192;

void FdioTest::SetUp() {
  loop_ = std::make_unique<async::Loop>(&kAsyncLoopConfigNoAttachToCurrentThread);

  auto device = std::make_unique<block_client::FakeBlockDevice>(kNumBlocks, kBlockSize);
  block_device_ = device.get();
  ASSERT_EQ(FormatFilesystem(block_device_,
                             FilesystemOptions{
                                 .blob_layout_format = GetBlobLayoutFormat(),
                                 .oldest_minor_version = GetOldestMinorVersion(),
                             }),
            ZX_OK);

  auto [outgoing_dir_client, outgoing_dir_server] =
      fidl::Endpoints<fuchsia_io::Directory>::Create();

  runner_ = std::make_unique<ComponentRunner>(
      *loop_, ComponentOptions{.pager_threads = mount_options_.paging_threads});
  ASSERT_EQ(runner_->ServeRoot(std::move(outgoing_dir_server), {}, std::move(vmex_resource_))
                .status_value(),
            ZX_OK);

  ASSERT_EQ(runner_->Configure(std::move(device), mount_options_).status_value(), ZX_OK);

  ASSERT_EQ(loop_->StartThread("blobfs test dispatcher"), ZX_OK);

  auto root_client_or = fs_management::FsRootHandle(outgoing_dir_client);
  ASSERT_EQ(root_client_or.status_value(), ZX_OK);

  // FDIO serving the root directory.
  ASSERT_EQ(
      fdio_fd_create(root_client_or->TakeChannel().release(), root_fd_.reset_and_get_address()),
      ZX_OK);
  ASSERT_TRUE(root_fd_.is_valid());
  ASSERT_EQ(fdio_fd_create(outgoing_dir_client.TakeChannel().release(),
                           outgoing_dir_fd_.reset_and_get_address()),
            ZX_OK);
  ASSERT_TRUE(outgoing_dir_fd_.is_valid());
}

void FdioTest::TearDown() {
  fdio_cpp::UnownedFdioCaller outgoing_dir(outgoing_dir_fd_);
  auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
  ASSERT_EQ(fidl::WireCall(outgoing_dir.directory())
                ->DeprecatedOpen(fuchsia_io::OpenFlags(0), {}, "svc",
                                 fidl::ServerEnd<fuchsia_io::Node>(endpoints.server.TakeChannel()))
                .status(),
            ZX_OK);
  auto admin_client = component::ConnectAt<fuchsia_fs::Admin>(endpoints.client);
  ASSERT_EQ(admin_client.status_value(), ZX_OK);
  ASSERT_EQ(fidl::WireCall(*admin_client)->Shutdown().status(), ZX_OK);
}

zx_handle_t FdioTest::outgoing_dir() {
  zx::channel outgoing_dir;
  fdio_fd_clone(outgoing_dir_fd_.get(), outgoing_dir.reset_and_get_address());
  return outgoing_dir.release();
}

void FdioTest::TakeSnapshot(inspect::Hierarchy* output) {
  async::Loop loop = async::Loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  loop.StartThread("metric-collection-thread");
  async::Executor executor(loop.dispatcher());

  async_dispatcher_t* dispatcher = executor.dispatcher();

  std::condition_variable cv;
  std::mutex m;
  bool done = false;
  {
    diagnostics::reader::ArchiveReader reader(dispatcher, {"test:root"});
    executor.schedule_task(reader.SnapshotInspectUntilPresent({"test"}).and_then(
        [&](std::vector<diagnostics::reader::InspectData>& data) {
          {
            std::unique_lock<std::mutex> lock(m);
            ASSERT_EQ(1ul, data.size());
            auto& inspect_data = data[0];
            if (!inspect_data.payload().has_value()) {
              FX_LOGS(INFO) << "inspect_data had nullopt payload";
            }
            if (inspect_data.payload().has_value() && inspect_data.payload().value() == nullptr) {
              FX_LOGS(INFO) << "inspect_data had nullptr for payload";
            }
            if (inspect_data.metadata().errors.has_value()) {
              for (const auto& e : inspect_data.metadata().errors.value()) {
                FX_LOGS(INFO) << e.message;
              }
            }

            ASSERT_TRUE(inspect_data.payload().has_value());
            *output = inspect_data.TakePayload();
            done = true;
          }
          cv.notify_all();
        }));

    std::unique_lock<std::mutex> lock(m);
    cv.wait(lock, [&] { return done; });
  }
  loop.Quit();
  loop.JoinThreads();
}

void FdioTest::GetUintMetricFromHierarchy(const inspect::Hierarchy& hierarchy,
                                          const std::vector<std::string>& path,
                                          const std::string& property, uint64_t* value) {
  ASSERT_NE(value, nullptr);
  const inspect::Hierarchy* direct_parent = hierarchy.GetByPath(path);
  ASSERT_NE(direct_parent, nullptr);

  const inspect::IntPropertyValue* property_node =
      direct_parent->node().get_property<inspect::IntPropertyValue>(property);
  ASSERT_NE(property_node, nullptr);

  *value = static_cast<uint64_t>(property_node->value());
}

void FdioTest::GetUintMetric(const std::vector<std::string>& path, const std::string& property,
                             uint64_t* value) {
  inspect::Hierarchy hierarchy;
  TakeSnapshot(&hierarchy);
  GetUintMetricFromHierarchy(hierarchy, path, property, value);
}

}  // namespace blobfs
