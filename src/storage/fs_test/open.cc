// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire_test_base.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/zx/channel.h>

#include <string>

#include <fbl/unique_fd.h>

#include "src/storage/fs_test/fs_test_fixture.h"
#include "src/storage/fs_test/misc.h"

namespace fs_test {
namespace {

namespace fio = fuchsia_io;

using OpenTest = FilesystemTest;

fidl::ClientEnd<fio::Directory> CreateDirectory(fio::wire::Flags dir_flags,
                                                const std::string& path) {
  EXPECT_EQ(mkdir(path.c_str(), 0755), 0);

  auto endpoints = fidl::Endpoints<fio::Directory>::Create();
  EXPECT_EQ(fdio_open3(path.c_str(),
                       static_cast<uint64_t>(dir_flags | fio::wire::Flags::kProtocolDirectory),
                       endpoints.server.TakeChannel().release()),
            ZX_OK);

  return std::move(endpoints.client);
}

zx_status_t OpenFileWithMaybeCreate(const fidl::ClientEnd<fio::Directory>& dir,
                                    const std::string& path) {
  fio::wire::Flags flags = fio::wire::Flags::kProtocolFile | fio::wire::Flags::kFlagMaybeCreate |
                           fio::wire::Flags::kFlagSendRepresentation;
  auto file_endpoints = fidl::Endpoints<fio::Node>::Create();
  auto open_result = fidl::WireCall(dir)->Open3(fidl::StringView::FromExternal(path), flags, {},
                                                file_endpoints.server.TakeChannel());
  EXPECT_EQ(open_result.status(), ZX_OK);
  fidl::WireSyncClient child{std::move(file_endpoints.client)};

  class EventHandler : public fidl::testing::WireSyncEventHandlerTestBase<fio::Node> {
   public:
    EventHandler() = default;
    zx_status_t status() const { return status_; }

    void OnRepresentation(fidl::WireEvent<fio::Node::OnRepresentation>* representation) override {
      EXPECT_EQ(representation->Which(), fio::wire::Representation::Tag::kFile);
      status_ = ZX_OK;
    }

    void NotImplemented_(const std::string& name) override { FAIL() << "Unexpected " << name; }

   private:
    zx_status_t status_ = ZX_OK;
  };

  EventHandler event_handler;
  auto status = child.HandleOneEvent(event_handler);
  if (!status.ok()) {
    EXPECT_NE(status.reason(), fidl::Reason::kUnexpectedMessage);
    return status.status();
  }
  return event_handler.status();
}

TEST_P(OpenTest, OpenFileWithCreateCreatesInReadWriteDir) {
  fio::wire::Flags flags = fio::wire::kPermReadable | fio::wire::kPermWritable;
  auto parent = CreateDirectory(flags, GetPath("a"));
  EXPECT_EQ(OpenFileWithMaybeCreate(parent, "b"), ZX_OK);
}

TEST_P(OpenTest, OpenFileWithCreateFailsInReadOnlyDir) {
  fio::wire::Flags flags = fio::wire::kPermReadable;
  auto parent = CreateDirectory(flags, GetPath("a"));
  EXPECT_EQ(OpenFileWithMaybeCreate(parent, "b"), ZX_ERR_ACCESS_DENIED);
}

TEST_P(OpenTest, OpenFileWithCreateCreatesInReadWriteDirWithPermInheritWrite) {
  fio::wire::Flags parent_flags = fio::wire::kPermReadable | fio::wire::kPermWritable;
  auto parent = CreateDirectory(parent_flags, GetPath("a"));

  // kPermInheritWrite expand the rights of the directory connection to include write rights if the
  // parent connection has them.
  fio::wire::Flags flags = fio::wire::kPermReadable | fio::wire::Flags::kPermInheritWrite |
                           fio::wire::Flags::kProtocolDirectory;
  std::string path = ".";
  auto endpoints = fidl::Endpoints<fio::Node>::Create();
  auto result = fidl::WireCall(parent)->Open3(fidl::StringView::FromExternal(path), flags, {},
                                              endpoints.server.TakeChannel());
  ASSERT_EQ(result.status(), ZX_OK);
  fidl::ClientEnd<fio::Directory> dir(endpoints.client.TakeChannel());

  // Should not be able to open a file with `dir` if the connection did not have write permissions.
  EXPECT_EQ(OpenFileWithMaybeCreate(dir, "b"), ZX_OK);
}

TEST_P(OpenTest, OpenFileWithCreateFailsInReadOnlyDirWithPermInheritWrite) {
  fio::wire::Flags parent_flags = fio::wire::kPermReadable;
  auto parent = CreateDirectory(parent_flags, GetPath("a"));

  // kPermInheritWrite expand the rights of the directory connection to include write rights if the
  // parent connection has them. As the parent directory only has read permissions, the resulting
  // connection will not have write permissions.
  fio::wire::Flags flags = fio::wire::kPermReadable | fio::wire::Flags::kPermInheritWrite |
                           fio::wire::Flags::kProtocolDirectory;
  std::string path = ".";
  auto clone_endpoints = fidl::Endpoints<fio::Node>::Create();
  auto clone_result = fidl::WireCall(parent)->Open3(fidl::StringView::FromExternal(path), flags, {},
                                                    clone_endpoints.server.TakeChannel());
  ASSERT_EQ(clone_result.status(), ZX_OK);
  fidl::ClientEnd<fio::Directory> clone_dir(clone_endpoints.client.TakeChannel());

  // Opening a file should fail as `clone_dir` as we expect the connection to not have write
  // permissions.
  EXPECT_EQ(OpenFileWithMaybeCreate(clone_dir, "b"), ZX_ERR_ACCESS_DENIED);
}

TEST_P(OpenTest, OpenFileWithCreateFailsInReadWriteDirPosixClone) {
  fio::wire::Flags flags = fio::wire::kPermReadable | fio::wire::kPermWritable;
  auto parent = CreateDirectory(flags, GetPath("a"));

  // kOpenFlagPosixWritable only does the rights expansion with the open call if the parent
  // connection has them.
  fio::wire::OpenFlags flags_deprecated = fio::wire::OpenFlags::kRightReadable |
                                          fio::wire::OpenFlags::kPosixWritable |
                                          fio::wire::OpenFlags::kDirectory;
  auto clone_endpoints = fidl::Endpoints<fio::Node>::Create();
  auto clone_res =
      fidl::WireCall(parent)->Clone(flags_deprecated, std::move(clone_endpoints.server));
  ASSERT_EQ(clone_res.status(), ZX_OK);
  fidl::ClientEnd<fio::Directory> clone_dir(clone_endpoints.client.TakeChannel());

  EXPECT_EQ(OpenFileWithMaybeCreate(clone_dir, "b"), ZX_ERR_ACCESS_DENIED);
}

TEST_P(OpenTest, OpenFileWithCreateFailsInReadOnlyDirPosixClone) {
  fio::wire::Flags flags = fio::wire::kPermReadable;
  auto parent = CreateDirectory(flags, GetPath("a"));

  fio::wire::OpenFlags flags_deprecated = fio::wire::OpenFlags::kRightReadable |
                                          fio::wire::OpenFlags::kPosixWritable |
                                          fio::wire::OpenFlags::kDirectory;
  auto clone_endpoints = fidl::Endpoints<fio::Node>::Create();
  auto clone_res =
      fidl::WireCall(parent)->Clone(flags_deprecated, std::move(clone_endpoints.server));
  ASSERT_EQ(clone_res.status(), ZX_OK);
  fidl::ClientEnd<fio::Directory> clone_dir(clone_endpoints.client.TakeChannel());

  EXPECT_EQ(OpenFileWithMaybeCreate(clone_dir, "b"), ZX_ERR_ACCESS_DENIED);
}

INSTANTIATE_TEST_SUITE_P(/*no prefix*/, OpenTest, testing::ValuesIn(AllTestFilesystems()),
                         testing::PrintToStringParamName());

}  // namespace
}  // namespace fs_test
