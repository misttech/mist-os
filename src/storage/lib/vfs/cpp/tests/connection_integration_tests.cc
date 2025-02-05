// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file includes basic VFS file/directory connection tests. For comprehensive behavioral tests,
// see the fuchsia.io Conformance Test Suite in //src/storage/conformance.

#include <fidl/fuchsia.io/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <stdio.h>
#include <zircon/errors.h>

#include <string_view>
#include <utility>
#include <vector>

#include <gtest/gtest.h>

#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/pseudo_file.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"

namespace {

namespace fio = fuchsia_io;

zx_status_t DummyReader(fbl::String* output) { return ZX_OK; }

zx_status_t DummyWriter(std::string_view input) { return ZX_OK; }

// Example vnode that supports protocol negotiation. Here the vnode may be opened as a file or a
// directory.
class FileOrDirectory : public fs::Vnode {
 public:
  FileOrDirectory() = default;

  fuchsia_io::NodeProtocolKinds GetProtocols() const final {
    return fuchsia_io::NodeProtocolKinds::kFile | fuchsia_io::NodeProtocolKinds::kDirectory;
  }

  fs::VnodeAttributesQuery SupportedMutableAttributes() const final {
    return fs::VnodeAttributesQuery::kModificationTime;
  }

  zx::result<fs::VnodeAttributes> GetAttributes() const final {
    return zx::ok(fs::VnodeAttributes{
        .id = 1234,
        .modification_time = modification_time_,
    });
  }

  bool ValidateRights(fio::Rights rights) const final {
    return (rights & fio::Rights::kExecute) == fio::Rights();
  }

  zx::result<> UpdateAttributes(const fs::VnodeAttributesUpdate& attributes) final {
    // Attributes not reported by |SupportedMutableAttributes()| should never be set in
    // |attributes|.
    ZX_ASSERT(!attributes.creation_time);
    modification_time_ = *attributes.modification_time;
    return zx::ok();
  }

 private:
  uint64_t modification_time_;
};

// Helper method to monitor the OnOpen event, used by the tests below when
// OPEN_FLAG_DESCRIBE is used.
zx::result<fio::wire::NodeInfoDeprecated> GetOnOpenResponse(
    fidl::UnownedClientEnd<fio::Node> channel) {
  class EventHandler final : public fidl::testing::WireSyncEventHandlerTestBase<fio::Node> {
   public:
    void OnOpen(fidl::WireEvent<fio::Node::OnOpen>* event) override {
      ASSERT_NE(event, nullptr);
      response = std::move(*event);
    }

    void NotImplemented_(const std::string& name) override {
      ADD_FAILURE() << "unexpected " << name.c_str();
    }

    fidl::WireEvent<fio::Node::OnOpen> response;
  };

  EventHandler event_handler;
  const fidl::Status result = event_handler.HandleOneEvent(channel);
  if (!result.ok()) {
    return zx::error(result.status());
  }
  fidl::WireEvent<fio::Node::OnOpen>& response = event_handler.response;
  if (response.s != ZX_OK) {
    return zx::error(response.s);
  }
  if (!response.info.has_value()) {
    return zx::error(ZX_ERR_BAD_STATE);
  }
  return zx::ok(std::move(response.info.value()));
}

// Helper method to monitor the OnRepresentation event. Used by the tests below to decode the
// fuchsia.io/Node.OnRepresentation event, or to check for the correct epitaph on errors.
zx::result<fio::Representation> GetOnRepresentation(fidl::UnownedClientEnd<fio::Node> channel) {
  struct EventHandler final : public fidl::testing::WireSyncEventHandlerTestBase<fio::Node> {
   public:
    void OnRepresentation(fidl::WireEvent<fio::Node::OnRepresentation>* event) override {
      ZX_ASSERT(event);
      representation = fidl::ToNatural(std::move(*event));
    }

    void NotImplemented_(const std::string& name) override {
      ADD_FAILURE() << "unexpected " << name;
    }

    std::optional<fio::Representation> representation = std::nullopt;
  };

  EventHandler event_handler;
  const fidl::Status result = event_handler.HandleOneEvent(channel);
  if (!result.ok()) {
    return zx::error(result.status());
  }
  ZX_ASSERT(event_handler.representation);
  return zx::ok(std::move(*event_handler.representation));
}

class VfsTestSetup : public testing::Test {
 public:
  // Setup file structure with one directory and one file. Note: On creation directories and files
  // have no flags and rights.
  VfsTestSetup() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
    vfs_.SetDispatcher(loop_.dispatcher());
    root_ = fbl::MakeRefCounted<fs::PseudoDir>();
    dir_ = fbl::MakeRefCounted<fs::PseudoDir>();
    file_ = fbl::MakeRefCounted<fs::BufferedPseudoFile>(&DummyReader, &DummyWriter);
    file_or_dir_ = fbl::MakeRefCounted<FileOrDirectory>();
    root_->AddEntry("dir", dir_);
    root_->AddEntry("file", file_);
    root_->AddEntry("file_or_dir", file_or_dir_);
  }

  zx_status_t ConnectClient(fidl::ServerEnd<fio::Directory> server_end) {
    // Serve root directory with maximum rights
    return vfs_.ServeDirectory(root_, std::move(server_end));
  }

  void SetReadonly() { vfs_.SetReadonly(true); }

 protected:
  void SetUp() override { loop_.StartThread(); }

  void TearDown() override { loop_.RunUntilIdle(); }

 private:
  async::Loop loop_;
  fs::SynchronousVfs vfs_;
  fbl::RefPtr<fs::PseudoDir> root_;
  fbl::RefPtr<fs::PseudoDir> dir_;
  fbl::RefPtr<fs::Vnode> file_;
  fbl::RefPtr<FileOrDirectory> file_or_dir_;
};

using ConnectionTest = VfsTestSetup;

TEST_F(ConnectionTest, NodeGetDeprecatedSetFlagsOnFile) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  // Connect to File
  zx::result fc = fidl::CreateEndpoints<fio::File>();
  ASSERT_EQ(fc.status_value(), ZX_OK);
  ASSERT_EQ(
      fdio_open3_at(root.client.channel().get(), "file", static_cast<uint64_t>(fio::kPermReadable),
                    fc->server.TakeChannel().release()),
      ZX_OK);

  // Use DeprecatedGetFlags to get current flags and rights
  auto file_get_result = fidl::WireCall(fc->client)->DeprecatedGetFlags();
  EXPECT_EQ(file_get_result.status(), ZX_OK);
  EXPECT_EQ(fio::OpenFlags::kRightReadable, file_get_result->flags);
  {
    // Make modifications to flags with DeprecatedSetFlags: Note this only works for
    // fio::OpenFlags::kAppend based on posix standard
    auto file_set_result = fidl::WireCall(fc->client)->DeprecatedSetFlags(fio::OpenFlags::kAppend);
    EXPECT_EQ(file_set_result->s, ZX_OK);
  }
  {
    // Check that the new flag is saved
    auto file_get_result = fidl::WireCall(fc->client)->DeprecatedGetFlags();
    EXPECT_EQ(file_get_result->s, ZX_OK);
    EXPECT_EQ(fio::OpenFlags::kRightReadable | fio::OpenFlags::kAppend, file_get_result->flags);
  }
}

TEST_F(ConnectionTest, NodeGetDeprecatedSetFlagsOnDirectory) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  // Connect to Directory
  zx::result dc = fidl::CreateEndpoints<fio::Directory>();
  ASSERT_EQ(dc.status_value(), ZX_OK);
  ASSERT_EQ(fdio_open3_at(root.client.channel().get(), "dir",
                          static_cast<uint64_t>(fio::kPermReadable | fio::kPermWritable),
                          dc->server.TakeChannel().release()),
            ZX_OK);

  // Directories don't have settable flags, only report RIGHT_* flags.
  auto dir_get_result = fidl::WireCall(dc->client)->DeprecatedGetFlags();
  EXPECT_EQ(dir_get_result->s, ZX_OK);
  EXPECT_EQ(fio::OpenFlags::kRightReadable | fio::OpenFlags::kRightWritable, dir_get_result->flags);

  // Directories do not support setting flags.
  auto dir_set_result = fidl::WireCall(dc->client)->DeprecatedSetFlags(fio::OpenFlags::kAppend);
  EXPECT_EQ(dir_set_result->s, ZX_ERR_NOT_SUPPORTED);
}

TEST_F(ConnectionTest, InheritPermissionFlagDirectoryRightExpansion) {
  // Create connection to VFS with all rights.
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  // Combinations of permission inherit flags to be tested.
  const fio::Flags kFlagCombinations[]{
      fio::Flags::kPermInheritWrite, fio::Flags::kPermInheritExecute,
      fio::Flags::kPermInheritWrite | fio::Flags::kPermInheritExecute};

  for (const fio::Flags kOpenFlags : kFlagCombinations) {
    // Connect to drectory specifying the flag combination we want to test.
    zx::result dc = fidl::CreateEndpoints<fio::Directory>();
    ASSERT_EQ(dc.status_value(), ZX_OK);
    ASSERT_EQ(fdio_open3_at(root.client.channel().get(), "dir",
                            static_cast<uint64_t>(fio::kPermReadable | kOpenFlags),
                            dc->server.TakeChannel().release()),
              ZX_OK);

    // Ensure flags match those which we expect.
    auto dir_get_result = fidl::WireCall(dc->client)->DeprecatedGetFlags();
    EXPECT_EQ(dir_get_result->s, ZX_OK);
    auto dir_flags = dir_get_result->flags;
    EXPECT_TRUE(fio::OpenFlags::kRightReadable & dir_flags);
    // Each permission inherit flag should be expanded to its respective right(s).
    if (kOpenFlags & fio::Flags::kPermInheritWrite)
      EXPECT_TRUE(fio::OpenFlags::kRightWritable & dir_flags);
    if (kOpenFlags & fio::Flags::kPermInheritExecute)
      EXPECT_TRUE(fio::OpenFlags::kRightExecutable & dir_flags);

    // Repeat test, but for file, which should not have any expanded rights.
    auto fc = fidl::Endpoints<fio::File>::Create();
    ASSERT_EQ(fdio_open3_at(root.client.channel().get(), "file",
                            static_cast<uint64_t>(fio::kPermReadable | kOpenFlags),
                            fc.server.TakeChannel().release()),
              ZX_OK);
    auto file_get_result = fidl::WireCall(fc.client)->DeprecatedGetFlags();
    EXPECT_EQ(file_get_result.status(), ZX_OK);
    EXPECT_EQ(fio::OpenFlags::kRightReadable, file_get_result->flags);
  }
}

TEST_F(ConnectionTest, FileGetDeprecatedSetFlagsOnFile) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  // Connect to File
  zx::result fc = fidl::CreateEndpoints<fio::File>();
  ASSERT_EQ(fc.status_value(), ZX_OK);
  ASSERT_EQ(
      fdio_open3_at(root.client.channel().get(), "file", static_cast<uint64_t>(fio::kPermReadable),
                    fc->server.TakeChannel().release()),
      ZX_OK);

  {
    // Use DeprecatedGetFlags to get current flags and rights
    auto file_get_result = fidl::WireCall(fc->client)->DeprecatedGetFlags();
    EXPECT_EQ(file_get_result.status(), ZX_OK);
    EXPECT_EQ(fio::OpenFlags::kRightReadable, file_get_result->flags);
  }
  {
    // Make modifications to flags with DeprecatedSetFlags: Note this only works for kOpenFlagAppend
    // based on posix standard
    auto file_set_result = fidl::WireCall(fc->client)->DeprecatedSetFlags(fio::OpenFlags::kAppend);
    EXPECT_EQ(file_set_result->s, ZX_OK);
  }
  {
    // Check that the new flag is saved
    auto file_get_result = fidl::WireCall(fc->client)->DeprecatedGetFlags();
    EXPECT_EQ(file_get_result->s, ZX_OK);
    EXPECT_EQ(fio::OpenFlags::kRightReadable | fio::OpenFlags::kAppend, file_get_result->flags);
  }
}

TEST_F(ConnectionTest, GetSetIo1Attrs) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  // Connect to File
  zx::result fc = fidl::CreateEndpoints<fio::File>();
  ASSERT_EQ(fc.status_value(), ZX_OK);
  ASSERT_EQ(fdio_open3_at(root.client.channel().get(), "file_or_dir",
                          static_cast<uint64_t>(fio::kPermReadable | fio::kPermWritable),
                          fc->server.TakeChannel().release()),
            ZX_OK);
  {
    auto io1_attrs = fidl::WireCall(fc->client)->GetAttr();
    ASSERT_EQ(io1_attrs.status(), ZX_OK);
    EXPECT_EQ(io1_attrs->attributes.modification_time, 0u);
  }

  // Ensure we can't set creation time.
  {
    auto io1_attrs =
        fidl::WireCall(fc->client)->SetAttr(fio::NodeAttributeFlags::kCreationTime, {});
    ASSERT_EQ(io1_attrs.status(), ZX_OK);
    // ASSERT_EQ(io1_attrs->s, ZX_ERR_NOT_SUPPORTED);
  }

  // Update modification time.
  {
    auto io1_attrs = fidl::WireCall(fc->client)
                         ->SetAttr(fio::NodeAttributeFlags::kModificationTime,
                                   fio::wire::NodeAttributes{.modification_time = 1234});
    ASSERT_EQ(io1_attrs.status(), ZX_OK);
    ASSERT_EQ(io1_attrs->s, ZX_OK);
  }

  // Check modification time was updated.
  {
    auto io1_attrs = fidl::WireCall(fc->client)->GetAttr();
    ASSERT_EQ(io1_attrs.status(), ZX_OK);
    EXPECT_EQ(io1_attrs->attributes.modification_time, 1234u);
  }
}

// Test that the io2 GetAttributes and UpdateAttributes methods work as expected.
TEST_F(ConnectionTest, GetUpdateIo2Attrs) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  // Connect to File
  zx::result fc = fidl::CreateEndpoints<fio::File>();
  ASSERT_EQ(fc.status_value(), ZX_OK);
  ASSERT_EQ(fdio_open3_at(root.client.channel().get(), "file_or_dir",
                          static_cast<uint64_t>(fio::kPermReadable | fio::kPermWritable),
                          fc->server.TakeChannel().release()),
            ZX_OK);
  auto client = fidl::SyncClient(std::move(fc->client));
  // Our test Vnode only reports a hard-coded ID in addition to protocols/abilities.
  fio::ImmutableNodeAttributes expected_immutable_attrs = fio::ImmutableNodeAttributes();
  expected_immutable_attrs.id() = 1234;
  expected_immutable_attrs.abilities() = FileOrDirectory().GetAbilities();
  expected_immutable_attrs.protocols() = FileOrDirectory().GetProtocols();
  // Our test Vnode only supports modification time, and should default-initialize it to zero.
  fio::MutableNodeAttributes expected_mutable_attrs = fio::MutableNodeAttributes();
  expected_mutable_attrs.modification_time() = 0;
  {
    auto attrs = client->GetAttributes(fio::NodeAttributesQuery::kMask);
    ASSERT_TRUE(attrs.is_ok());
    EXPECT_EQ(attrs->immutable_attributes(), expected_immutable_attrs);
    EXPECT_EQ(attrs->mutable_attributes(), expected_mutable_attrs);
  }

  // Ensure we can't set creation time.
  {
    fio::MutableNodeAttributes update = fio::MutableNodeAttributes();
    update.creation_time() = 0;
    auto result = client->UpdateAttributes(update);
    ASSERT_TRUE(result.is_error());
    EXPECT_EQ(result.error_value().domain_error(), ZX_ERR_NOT_SUPPORTED);
  }

  // Update modification time.
  expected_mutable_attrs.modification_time() = 1234;
  {
    auto result = client->UpdateAttributes(expected_mutable_attrs);
    ASSERT_TRUE(result.is_ok());
  }

  // Check modification time was updated and other attributes remain unchanged.
  {
    auto attrs = client->GetAttributes(fio::NodeAttributesQuery::kMask);
    ASSERT_TRUE(attrs.is_ok());
    EXPECT_EQ(attrs->immutable_attributes(), expected_immutable_attrs);
    EXPECT_EQ(attrs->mutable_attributes(), expected_mutable_attrs);
  }
}

TEST_F(ConnectionTest, FileSeekDirectory) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  // Interacting with a Directory connection using File protocol methods should fail.
  {
    zx::result dc = fidl::CreateEndpoints<fio::Directory>();
    ASSERT_EQ(dc.status_value(), ZX_OK);
    ASSERT_EQ(fdio_open3_at(root.client.channel().get(), "dir",
                            static_cast<uint64_t>(fio::kPermReadable | fio::kPermWritable),
                            dc->server.TakeChannel().release()),
              ZX_OK);

    // Borrowing directory channel as file channel.
    auto dir_get_result =
        fidl::WireCall(fidl::UnownedClientEnd<fio::File>(dc->client.borrow().handle()))
            ->Seek(fio::wire::SeekOrigin::kStart, 0);
    EXPECT_NE(dir_get_result.status(), ZX_OK);
  }
}

TEST_F(ConnectionTest, NegotiateProtocol) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  // Connect to polymorphic node as a directory, by passing |kOpenFlagDirectory|.
  {
    auto [dir_client, dir_server] = fidl::Endpoints<fio::Node>::Create();
    ASSERT_EQ(fidl::WireCall(root.client)
                  ->Open(fio::OpenFlags::kRightReadable | fio::OpenFlags::kDescribe |
                             fio::OpenFlags::kDirectory,
                         {}, fidl::StringView("file_or_dir"), std::move(dir_server))
                  .status(),
              ZX_OK);
    zx::result<fio::wire::NodeInfoDeprecated> dir_info = GetOnOpenResponse(dir_client);
    ASSERT_EQ(dir_info.status_value(), ZX_OK);
    ASSERT_TRUE(dir_info->is_directory());
    // Check that if we clone the connection, we still get the directory protocol.
    auto [clone_client, clone_server] = fidl::Endpoints<fio::Node>::Create();
    ASSERT_EQ(fidl::WireCall(dir_client)
                  ->DeprecatedClone(fio::OpenFlags::kDescribe | fio::OpenFlags::kCloneSameRights,
                                    std::move(clone_server))
                  .status(),
              ZX_OK);
    zx::result<fio::wire::NodeInfoDeprecated> cloned_info = GetOnOpenResponse(clone_client);
    ASSERT_EQ(cloned_info.status_value(), ZX_OK);
    ASSERT_TRUE(cloned_info->is_directory());
  }
  {
    // Connect to polymorphic node as a file, by passing |kOpenFlagNotDirectory|.
    auto [file_client, file_server] = fidl::Endpoints<fio::Node>::Create();
    ASSERT_EQ(fidl::WireCall(root.client)
                  ->Open(fio::OpenFlags::kRightReadable | fio::OpenFlags::kDescribe |
                             fio::OpenFlags::kNotDirectory,
                         {}, fidl::StringView("file_or_dir"), std::move(file_server))
                  .status(),
              ZX_OK);
    zx::result<fio::wire::NodeInfoDeprecated> file_info = GetOnOpenResponse(file_client);
    ASSERT_EQ(file_info.status_value(), ZX_OK);
    ASSERT_TRUE(file_info->is_file());
    // Check that if we clone the connection, we still get the file protocol.
    auto [clone_client, clone_server] = fidl::Endpoints<fio::Node>::Create();
    ASSERT_EQ(fidl::WireCall(file_client)
                  ->DeprecatedClone(fio::OpenFlags::kDescribe | fio::OpenFlags::kCloneSameRights,
                                    std::move(clone_server))
                  .status(),
              ZX_OK);
    zx::result<fio::wire::NodeInfoDeprecated> cloned_info = GetOnOpenResponse(clone_client);
    ASSERT_EQ(cloned_info.status_value(), ZX_OK);
    ASSERT_TRUE(cloned_info->is_file());
  }
  {
    // Connect to polymorphic node as a node reference, by passing |kNodeReference|.
    auto [node_client, node_server] = fidl::Endpoints<fio::Node>::Create();
    ASSERT_EQ(fidl::WireCall(root.client)
                  ->Open(fio::OpenFlags::kNodeReference | fio::OpenFlags::kDescribe, {},
                         fidl::StringView("file_or_dir"), std::move(node_server))
                  .status(),
              ZX_OK);
    zx::result<fio::wire::NodeInfoDeprecated> node_info = GetOnOpenResponse(node_client);
    ASSERT_EQ(node_info.status_value(), ZX_OK);
    // In io1, node reference connections map to the service representation in the OnOpen event.
    ASSERT_TRUE(node_info->is_service());
    // Check that if we clone the connection, we still get the node protocol.
    auto [clone_client, clone_server] = fidl::Endpoints<fio::Node>::Create();
    ASSERT_EQ(fidl::WireCall(node_client)
                  ->DeprecatedClone(fio::OpenFlags::kDescribe | fio::OpenFlags::kCloneSameRights,
                                    std::move(clone_server))
                  .status(),
              ZX_OK);
    zx::result<fio::wire::NodeInfoDeprecated> cloned_info = GetOnOpenResponse(clone_client);
    ASSERT_EQ(cloned_info.status_value(), ZX_OK);
    ASSERT_TRUE(cloned_info->is_service());
  }
}

TEST_F(ConnectionTest, NegotiateProtocolOpen3) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  {
    // Connect to polymorphic node as a directory.
    zx::result dc = fidl::CreateEndpoints<fio::Node>();
    ASSERT_EQ(dc.status_value(), ZX_OK);
    ASSERT_EQ(fidl::WireCall(root.client)
                  ->Open3(fidl::StringView("file_or_dir"),
                          fio::Flags::kProtocolDirectory | fio::Flags::kFlagSendRepresentation, {},
                          dc->server.TakeChannel())
                  .status(),
              ZX_OK);
    zx::result<fio::Representation> dir_info = GetOnRepresentation(dc->client);
    ASSERT_EQ(dir_info.status_value(), ZX_OK);
    ASSERT_EQ(dir_info->Which(), fio::Representation::Tag::kDirectory);
  }

  {
    // Connect to polymorphic node as a file.
    zx::result fc = fidl::CreateEndpoints<fio::Node>();
    ASSERT_EQ(fc.status_value(), ZX_OK);
    ASSERT_EQ(fidl::WireCall(root.client)
                  ->Open3(fidl::StringView("file_or_dir"),
                          fio::Flags::kProtocolFile | fio::Flags::kFlagSendRepresentation, {},
                          fc->server.TakeChannel())
                  .status(),
              ZX_OK);
    zx::result<fio::Representation> file_info = GetOnRepresentation(fc->client);
    ASSERT_EQ(file_info.status_value(), ZX_OK);
    ASSERT_EQ(file_info->Which(), fio::Representation::Tag::kFile);
  }
}

TEST_F(ConnectionTest, PrevalidateFlagsOpenFailure) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  // The only invalid flag combination for fuchsia.io/Node.Clone is specifying CLONE_SAME_RIGHTS
  // with any other rights.
  constexpr fio::OpenFlags kInvalidFlagCombo =
      fio::OpenFlags::kCloneSameRights | fio::OpenFlags::kRightReadable | fio::OpenFlags::kDescribe;
  zx::result dc = fidl::CreateEndpoints<fio::Node>();
  ASSERT_EQ(dc.status_value(), ZX_OK);
  // Ensure that invalid flag combination returns INVALID_ARGS.
  ASSERT_EQ(
      fidl::WireCall(root.client)
          ->Open(kInvalidFlagCombo, {}, fidl::StringView("file_or_dir"), std::move(dc->server))
          .status(),
      ZX_OK);
  ASSERT_EQ(GetOnOpenResponse(dc->client).status_value(), ZX_ERR_INVALID_ARGS);
}

TEST_F(ConnectionTest, ValidateRights) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);
  // The test Vnode should disallow execute rights.
  {
    zx::result fc = fidl::CreateEndpoints<fio::Node>();
    ASSERT_EQ(fc.status_value(), ZX_OK);
    ASSERT_EQ(fidl::WireCall(root.client)
                  ->Open3(fidl::StringView("file_or_dir"),
                          fio::Flags::kFlagSendRepresentation | fio::Flags::kProtocolFile |
                              fio::Flags::kPermExecute,
                          {}, fc->server.TakeChannel())
                  .status(),
              ZX_OK);
    zx::result<fio::Representation> file_info = GetOnRepresentation(fc->client);
    ASSERT_EQ(file_info.status_value(), ZX_ERR_ACCESS_DENIED);
  }
}

TEST_F(ConnectionTest, ValidateRightsReadonly) {
  // Set the filesystem as read-only before creating a root connection.
  SetReadonly();
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  {
    // If the filesystem is read only, we shouldn't be able to open files as writable.
    zx::result fc = fidl::CreateEndpoints<fio::Node>();
    ASSERT_EQ(fc.status_value(), ZX_OK);
    ASSERT_EQ(fidl::WireCall(root.client)
                  ->Open3(fidl::StringView("file_or_dir"),
                          fio::Flags::kFlagSendRepresentation | fio::Flags::kProtocolFile |
                              fio::Flags::kPermWrite,
                          {}, fc->server.TakeChannel())
                  .status(),
              ZX_OK);
    zx::result<fio::Representation> file_info = GetOnRepresentation(fc->client);
    ASSERT_TRUE(file_info.is_error());
    ASSERT_EQ(file_info.error_value(), ZX_ERR_ACCESS_DENIED);
  }
  {
    // If the filesystem is read only, we shouldn't be granted mutable rights for directories.
    zx::result fc = fidl::CreateEndpoints<fio::Node>();
    ASSERT_EQ(fc.status_value(), ZX_OK);
    ASSERT_EQ(fidl::WireCall(root.client)
                  ->Open3(fidl::StringView("file_or_dir"),
                          fio::Flags::kFlagSendRepresentation | fio::Flags::kProtocolDirectory |
                              fio::Flags::kPermGetAttributes | fio::Flags::kPermInheritWrite,
                          {}, fc->server.TakeChannel())
                  .status(),
              ZX_OK);
    zx::result<fio::Representation> dir_info = GetOnRepresentation(fc->client);
    ASSERT_EQ(dir_info.status_value(), ZX_OK);
    ASSERT_EQ(dir_info->Which(), fio::Representation::Tag::kDirectory);
    auto connection_info = fidl::WireCall(fc->client)->GetConnectionInfo();
    ASSERT_EQ(connection_info.status(), ZX_OK);
    ASSERT_TRUE(connection_info->has_rights());
    ASSERT_EQ(connection_info->rights() & fs::kAllMutableIo2Rights, fio::Rights());
  }
}

// A vnode which maintains a counter of number of |Open| calls that have not been balanced out with
// a |Close|.
class CountOutstandingOpenVnode : public fs::Vnode {
 public:
  CountOutstandingOpenVnode() = default;

  fuchsia_io::NodeProtocolKinds GetProtocols() const final {
    return fuchsia_io::NodeProtocolKinds::kFile;
  }

  uint64_t GetOpenCount() const {
    std::lock_guard lock(mutex_);
    return open_count();
  }
};

class ConnectionClosingTest : public testing::Test {
 public:
  // Setup file structure with one directory and one file. Note: On creation directories and files
  // have no flags and rights.
  ConnectionClosingTest() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
    vfs_.SetDispatcher(loop_.dispatcher());
    root_ = fbl::MakeRefCounted<fs::PseudoDir>();
    count_outstanding_open_vnode_ = fbl::MakeRefCounted<CountOutstandingOpenVnode>();
    root_->AddEntry("count_outstanding_open_vnode", count_outstanding_open_vnode_);
  }

  zx_status_t ConnectClient(fidl::ServerEnd<fuchsia_io::Directory> server_end) {
    // Serve root directory with maximum rights
    return vfs_.ServeDirectory(root_, std::move(server_end));
  }

 protected:
  fbl::RefPtr<CountOutstandingOpenVnode> count_outstanding_open_vnode() const {
    return count_outstanding_open_vnode_;
  }

  async::Loop& loop() { return loop_; }

 private:
  async::Loop loop_;
  fs::SynchronousVfs vfs_;
  fbl::RefPtr<fs::PseudoDir> root_;
  fbl::RefPtr<CountOutstandingOpenVnode> count_outstanding_open_vnode_;
};

TEST_F(ConnectionClosingTest, ClosingChannelImpliesClosingNode) {
  // Create connection to vfs.
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  constexpr unsigned kNumActiveClients = 20;

  ASSERT_EQ(count_outstanding_open_vnode()->GetOpenCount(), 0u);

  // Create a number of active connections to "count_outstanding_open_vnode".
  std::vector<fidl::ClientEnd<fio::Node>> clients;
  for (unsigned i = 0; i < kNumActiveClients; i++) {
    zx::result fc = fidl::CreateEndpoints<fio::Node>();
    ASSERT_EQ(fc.status_value(), ZX_OK);
    ASSERT_EQ(fidl::WireCall(root.client)
                  ->Open(fio::OpenFlags::kRightReadable, {},
                         fidl::StringView("count_outstanding_open_vnode"), std::move(fc->server))
                  .status(),
              ZX_OK);
    clients.push_back(std::move(fc->client));
  }

  ASSERT_EQ(loop().RunUntilIdle(), ZX_OK);
  ASSERT_EQ(count_outstanding_open_vnode()->GetOpenCount(), kNumActiveClients);

  // Drop all the clients, leading to |Close| being invoked on "count_outstanding_open_vnode"
  // eventually.
  clients.clear();

  ASSERT_EQ(loop().RunUntilIdle(), ZX_OK);
  ASSERT_EQ(count_outstanding_open_vnode()->GetOpenCount(), 0u);
}

TEST_F(ConnectionClosingTest, ClosingNodeLeadsToClosingServerEndChannel) {
  // Create connection to vfs.
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_EQ(ConnectClient(std::move(root.server)), ZX_OK);

  zx_signals_t observed = ZX_SIGNAL_NONE;
  ASSERT_EQ(ZX_ERR_TIMED_OUT, root.client.channel().wait_one(ZX_CHANNEL_PEER_CLOSED,
                                                             zx::time::infinite_past(), &observed));
  ASSERT_FALSE(observed & ZX_CHANNEL_PEER_CLOSED);

  ASSERT_EQ(loop().StartThread(), ZX_OK);
  auto result = fidl::WireCall(root.client)->Close();
  ASSERT_EQ(result.status(), ZX_OK);
  ASSERT_TRUE(result->is_ok()) << zx_status_get_string(result->error_value());

  observed = ZX_SIGNAL_NONE;
  ASSERT_EQ(root.client.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(), &observed),
            ZX_OK);
  ASSERT_TRUE(observed & ZX_CHANNEL_PEER_CLOSED);

  loop().Shutdown();
}

}  // namespace
