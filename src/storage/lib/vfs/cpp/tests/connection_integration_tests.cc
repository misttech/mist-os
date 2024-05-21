// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
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

#include <zxtest/zxtest.h>

#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/pseudo_file.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"

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
        .modification_time = modification_time_,
    });
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
      ADD_FAILURE("unexpected %s", name.c_str());
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
  // In the success case, dispatch a trivial method to synchronize with the VFS; at the time of
  // writing, the VFS implementation sends the event *before* starting to service the channel. This
  // can lead to races with test teardown where binding the channel happens after the dispatcher has
  // been shutdown, which results in a panic in the FIDL runtime.
  {
    const fidl::WireResult result = fidl::WireCall(channel)->GetFlags();
    zx_status_t status = result.ok() ? zx_status_t{result.value().s} : result.status();
    if (status != ZX_OK) {
      ADD_FAILURE("fuchisa.io/Node.GetFlags failed unexpectedly: %s", zx_status_get_string(status));
      return zx::error(status);
    }
  }
  return zx::ok(std::move(response.info.value()));
}

class VfsTestSetup : public zxtest::Test {
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

 protected:
  void SetUp() override { loop_.StartThread(); }

  void TearDown() override { loop_.Shutdown(); }

 private:
  async::Loop loop_;
  fs::SynchronousVfs vfs_;
  fbl::RefPtr<fs::PseudoDir> root_;
  fbl::RefPtr<fs::PseudoDir> dir_;
  fbl::RefPtr<fs::Vnode> file_;
  fbl::RefPtr<FileOrDirectory> file_or_dir_;
};

using ConnectionTest = VfsTestSetup;

TEST_F(ConnectionTest, NodeGetSetFlagsOnFile) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_OK(ConnectClient(std::move(root.server)));

  // Connect to File
  zx::result fc = fidl::CreateEndpoints<fio::File>();
  ASSERT_OK(fc.status_value());
  ASSERT_OK(fdio_open_at(root.client.channel().get(), "file",
                         static_cast<uint32_t>(fio::OpenFlags::kRightReadable),
                         fc->server.TakeChannel().release()));

  // Use GetFlags to get current flags and rights
  auto file_get_result = fidl::WireCall(fc->client)->GetFlags();
  EXPECT_OK(file_get_result.status());
  EXPECT_EQ(fio::OpenFlags::kRightReadable, file_get_result->flags);
  {
    // Make modifications to flags with SetFlags: Note this only works for kOpenFlagAppend
    // based on posix standard
    auto file_set_result = fidl::WireCall(fc->client)->SetFlags(fio::OpenFlags::kAppend);
    EXPECT_OK(file_set_result->s);
  }
  {
    // Check that the new flag is saved
    auto file_get_result = fidl::WireCall(fc->client)->GetFlags();
    EXPECT_OK(file_get_result->s);
    EXPECT_EQ(fio::OpenFlags::kRightReadable | fio::OpenFlags::kAppend, file_get_result->flags);
  }
}

TEST_F(ConnectionTest, NodeGetSetFlagsOnDirectory) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_OK(ConnectClient(std::move(root.server)));

  // Connect to Directory
  zx::result dc = fidl::CreateEndpoints<fio::Directory>();
  ASSERT_OK(dc.status_value());
  ASSERT_OK(fdio_open_at(
      root.client.channel().get(), "dir",
      static_cast<uint32_t>(fio::OpenFlags::kRightReadable | fio::OpenFlags::kRightWritable),
      dc->server.TakeChannel().release()));

  // Directories don't have settable flags, only report RIGHT_* flags.
  auto dir_get_result = fidl::WireCall(dc->client)->GetFlags();
  EXPECT_OK(dir_get_result->s);
  EXPECT_EQ(fio::OpenFlags::kRightReadable | fio::OpenFlags::kRightWritable, dir_get_result->flags);

  // Directories do not support setting flags.
  auto dir_set_result = fidl::WireCall(dc->client)->SetFlags(fio::OpenFlags::kAppend);
  EXPECT_EQ(dir_set_result->s, ZX_ERR_NOT_SUPPORTED);
}

TEST_F(ConnectionTest, PosixFlagDirectoryRightExpansion) {
  // Create connection to VFS with all rights.
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_OK(ConnectClient(std::move(root.server)));

  // Combinations of POSIX flags to be tested.
  const fio::OpenFlags OPEN_FLAG_COMBINATIONS[]{
      fio::OpenFlags::kPosixWritable, fio::OpenFlags::kPosixExecutable,
      fio::OpenFlags::kPosixWritable | fio::OpenFlags::kPosixExecutable};

  for (const fio::OpenFlags OPEN_FLAGS : OPEN_FLAG_COMBINATIONS) {
    // Connect to drectory specifying the flag combination we want to test.
    zx::result dc = fidl::CreateEndpoints<fio::Directory>();
    ASSERT_OK(dc.status_value());
    ASSERT_OK(fdio_open_at(root.client.channel().get(), "dir",
                           static_cast<uint32_t>(fio::OpenFlags::kRightReadable | OPEN_FLAGS),
                           dc->server.TakeChannel().release()));

    // Ensure flags match those which we expect.
    auto dir_get_result = fidl::WireCall(dc->client)->GetFlags();
    EXPECT_OK(dir_get_result->s);
    auto dir_flags = dir_get_result->flags;
    EXPECT_TRUE(fio::OpenFlags::kRightReadable & dir_flags);
    // Each POSIX flag should be expanded to its respective right(s).
    if (OPEN_FLAGS & fio::OpenFlags::kPosixWritable)
      EXPECT_TRUE(fio::OpenFlags::kRightWritable & dir_flags);
    if (OPEN_FLAGS & fio::OpenFlags::kPosixExecutable)
      EXPECT_TRUE(fio::OpenFlags::kRightExecutable & dir_flags);

    // Repeat test, but for file, which should not have any expanded rights.
    auto fc = fidl::Endpoints<fio::File>::Create();
    ASSERT_OK(fdio_open_at(root.client.channel().get(), "file",
                           static_cast<uint32_t>(fio::OpenFlags::kRightReadable | OPEN_FLAGS),
                           fc.server.TakeChannel().release()));
    auto file_get_result = fidl::WireCall(fc.client)->GetFlags();
    EXPECT_OK(file_get_result.status());
    EXPECT_EQ(fio::OpenFlags::kRightReadable, file_get_result->flags);
  }
}

TEST_F(ConnectionTest, FileGetSetFlagsOnFile) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_OK(ConnectClient(std::move(root.server)));

  // Connect to File
  zx::result fc = fidl::CreateEndpoints<fio::File>();
  ASSERT_OK(fc.status_value());
  ASSERT_OK(fdio_open_at(root.client.channel().get(), "file",
                         static_cast<uint32_t>(fio::OpenFlags::kRightReadable),
                         fc->server.TakeChannel().release()));

  {
    // Use GetFlags to get current flags and rights
    auto file_get_result = fidl::WireCall(fc->client)->GetFlags();
    EXPECT_OK(file_get_result.status());
    EXPECT_EQ(fio::OpenFlags::kRightReadable, file_get_result->flags);
  }
  {
    // Make modifications to flags with SetFlags: Note this only works for kOpenFlagAppend
    // based on posix standard
    auto file_set_result = fidl::WireCall(fc->client)->SetFlags(fio::OpenFlags::kAppend);
    EXPECT_OK(file_set_result->s);
  }
  {
    // Check that the new flag is saved
    auto file_get_result = fidl::WireCall(fc->client)->GetFlags();
    EXPECT_OK(file_get_result->s);
    EXPECT_EQ(fio::OpenFlags::kRightReadable | fio::OpenFlags::kAppend, file_get_result->flags);
  }
}

// TODO(https://fxbug.dev/340626555): Add equivalent io2 test for GetAttributes/UpdateAttributes
// when these methods are supported by the C++ VFS.
TEST_F(ConnectionTest, GetSetIo1Attrs) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_OK(ConnectClient(std::move(root.server)));

  // Connect to File
  zx::result fc = fidl::CreateEndpoints<fio::File>();
  ASSERT_OK(fc.status_value());
  ASSERT_OK(fdio_open_at(
      root.client.channel().get(), "file_or_dir",
      static_cast<uint32_t>(fio::OpenFlags::kRightReadable | fio::OpenFlags::kRightWritable),
      fc->server.TakeChannel().release()));
  {
    auto io1_attrs = fidl::WireCall(fc->client)->GetAttr();
    ASSERT_OK(io1_attrs.status());
    EXPECT_EQ(io1_attrs->attributes.modification_time, 0);
  }

  // Ensure we can't set creation time.
  {
    auto io1_attrs =
        fidl::WireCall(fc->client)->SetAttr(fio::NodeAttributeFlags::kCreationTime, {});
    ASSERT_OK(io1_attrs.status());
    // ASSERT_EQ(io1_attrs->s, ZX_ERR_NOT_SUPPORTED);
  }

  // Update modification time.
  {
    auto io1_attrs = fidl::WireCall(fc->client)
                         ->SetAttr(fio::NodeAttributeFlags::kModificationTime,
                                   fio::wire::NodeAttributes{.modification_time = 1234});
    ASSERT_OK(io1_attrs.status());
    ASSERT_EQ(io1_attrs->s, ZX_OK);
  }

  // Check modification time was updated.
  {
    auto io1_attrs = fidl::WireCall(fc->client)->GetAttr();
    ASSERT_OK(io1_attrs.status());
    EXPECT_EQ(io1_attrs->attributes.modification_time, 1234);
  }
}

TEST_F(ConnectionTest, FileSeekDirectory) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_OK(ConnectClient(std::move(root.server)));

  // Interacting with a Directory connection using File protocol methods should fail.
  {
    zx::result dc = fidl::CreateEndpoints<fio::Directory>();
    ASSERT_OK(dc.status_value());
    ASSERT_OK(fdio_open_at(
        root.client.channel().get(), "dir",
        static_cast<uint32_t>(fio::OpenFlags::kRightReadable | fio::OpenFlags::kRightWritable),
        dc->server.TakeChannel().release()));

    // Borrowing directory channel as file channel.
    auto dir_get_result =
        fidl::WireCall(fidl::UnownedClientEnd<fio::File>(dc->client.borrow().handle()))
            ->Seek(fio::wire::SeekOrigin::kStart, 0);
    EXPECT_NOT_OK(dir_get_result.status());
  }
}

TEST_F(ConnectionTest, NegotiateProtocol) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_OK(ConnectClient(std::move(root.server)));

  // Connect to polymorphic node as a directory, by passing |kOpenFlagDirectory|.
  {
    auto [dir_client, dir_server] = fidl::Endpoints<fio::Node>::Create();
    ASSERT_OK(fidl::WireCall(root.client)
                  ->Open(fio::OpenFlags::kRightReadable | fio::OpenFlags::kDescribe |
                             fio::OpenFlags::kDirectory,
                         {}, fidl::StringView("file_or_dir"), std::move(dir_server))
                  .status());
    zx::result<fio::wire::NodeInfoDeprecated> dir_info = GetOnOpenResponse(dir_client);
    ASSERT_OK(dir_info);
    ASSERT_TRUE(dir_info->is_directory());
    // Check that if we clone the connection, we still get the directory protocol.
    auto [clone_client, clone_server] = fidl::Endpoints<fio::Node>::Create();
    ASSERT_OK(fidl::WireCall(dir_client)
                  ->Clone(fio::OpenFlags::kDescribe | fio::OpenFlags::kCloneSameRights,
                          std::move(clone_server))
                  .status());
    zx::result<fio::wire::NodeInfoDeprecated> cloned_info = GetOnOpenResponse(clone_client);
    ASSERT_OK(cloned_info);
    ASSERT_TRUE(cloned_info->is_directory());
  }
  {
    // Connect to polymorphic node as a file, by passing |kOpenFlagNotDirectory|.
    auto [file_client, file_server] = fidl::Endpoints<fio::Node>::Create();
    ASSERT_OK(fidl::WireCall(root.client)
                  ->Open(fio::OpenFlags::kRightReadable | fio::OpenFlags::kDescribe |
                             fio::OpenFlags::kNotDirectory,
                         {}, fidl::StringView("file_or_dir"), std::move(file_server))
                  .status());
    zx::result<fio::wire::NodeInfoDeprecated> file_info = GetOnOpenResponse(file_client);
    ASSERT_OK(file_info);
    ASSERT_TRUE(file_info->is_file());
    // Check that if we clone the connection, we still get the file protocol.
    auto [clone_client, clone_server] = fidl::Endpoints<fio::Node>::Create();
    ASSERT_OK(fidl::WireCall(file_client)
                  ->Clone(fio::OpenFlags::kDescribe | fio::OpenFlags::kCloneSameRights,
                          std::move(clone_server))
                  .status());
    zx::result<fio::wire::NodeInfoDeprecated> cloned_info = GetOnOpenResponse(clone_client);
    ASSERT_OK(cloned_info);
    ASSERT_TRUE(cloned_info->is_file());
  }
  {
    // Connect to polymorphic node as a node reference, by passing |kNodeReference|.
    auto [node_client, node_server] = fidl::Endpoints<fio::Node>::Create();
    ASSERT_OK(fidl::WireCall(root.client)
                  ->Open(fio::OpenFlags::kNodeReference | fio::OpenFlags::kDescribe, {},
                         fidl::StringView("file_or_dir"), std::move(node_server))
                  .status());
    zx::result<fio::wire::NodeInfoDeprecated> node_info = GetOnOpenResponse(node_client);
    ASSERT_OK(node_info);
    // In io1, node reference connections map to the service representation in the OnOpen event.
    ASSERT_TRUE(node_info->is_service());
    // Check that if we clone the connection, we still get the node protocol.
    auto [clone_client, clone_server] = fidl::Endpoints<fio::Node>::Create();
    ASSERT_OK(fidl::WireCall(node_client)
                  ->Clone(fio::OpenFlags::kDescribe | fio::OpenFlags::kCloneSameRights,
                          std::move(clone_server))
                  .status());
    zx::result<fio::wire::NodeInfoDeprecated> cloned_info = GetOnOpenResponse(clone_client);
    ASSERT_OK(cloned_info);
    ASSERT_TRUE(cloned_info->is_service());
  }
}

TEST_F(ConnectionTest, PrevalidateFlagsOpenFailure) {
  // Create connection to vfs
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_OK(ConnectClient(std::move(root.server)));

  // The only invalid flag combination for fuchsia.io/Node.Clone is specifying CLONE_SAME_RIGHTS
  // with any other rights.
  constexpr fio::OpenFlags kInvalidFlagCombo =
      fio::OpenFlags::kCloneSameRights | fio::OpenFlags::kRightReadable | fio::OpenFlags::kDescribe;
  zx::result dc = fidl::CreateEndpoints<fio::Node>();
  ASSERT_OK(dc.status_value());
  // Ensure that invalid flag combination returns INVALID_ARGS.
  ASSERT_OK(
      fidl::WireCall(root.client)
          ->Open(kInvalidFlagCombo, {}, fidl::StringView("file_or_dir"), std::move(dc->server))
          .status());
  ASSERT_EQ(GetOnOpenResponse(dc->client).status_value(), ZX_ERR_INVALID_ARGS);
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

class ConnectionClosingTest : public zxtest::Test {
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
  ASSERT_OK(ConnectClient(std::move(root.server)));

  constexpr int kNumActiveClients = 20;

  ASSERT_EQ(count_outstanding_open_vnode()->GetOpenCount(), 0);

  // Create a number of active connections to "count_outstanding_open_vnode".
  std::vector<fidl::ClientEnd<fio::Node>> clients;
  for (int i = 0; i < kNumActiveClients; i++) {
    zx::result fc = fidl::CreateEndpoints<fio::Node>();
    ASSERT_OK(fc.status_value());
    ASSERT_OK(fidl::WireCall(root.client)
                  ->Open(fio::OpenFlags::kRightReadable, {},
                         fidl::StringView("count_outstanding_open_vnode"), std::move(fc->server))
                  .status());
    clients.push_back(std::move(fc->client));
  }

  ASSERT_OK(loop().RunUntilIdle());
  ASSERT_EQ(count_outstanding_open_vnode()->GetOpenCount(), kNumActiveClients);

  // Drop all the clients, leading to |Close| being invoked on "count_outstanding_open_vnode"
  // eventually.
  clients.clear();

  ASSERT_OK(loop().RunUntilIdle());
  ASSERT_EQ(count_outstanding_open_vnode()->GetOpenCount(), 0);
}

TEST_F(ConnectionClosingTest, ClosingNodeLeadsToClosingServerEndChannel) {
  // Create connection to vfs.
  auto root = fidl::Endpoints<fio::Directory>::Create();
  ASSERT_OK(ConnectClient(std::move(root.server)));

  zx_signals_t observed = ZX_SIGNAL_NONE;
  ASSERT_STATUS(
      ZX_ERR_TIMED_OUT,
      root.client.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite_past(), &observed));
  ASSERT_FALSE(observed & ZX_CHANNEL_PEER_CLOSED);

  ASSERT_OK(loop().StartThread());
  auto result = fidl::WireCall(root.client)->Close();
  ASSERT_OK(result.status());
  ASSERT_TRUE(result->is_ok(), "%s", zx_status_get_string(result->error_value()));

  observed = ZX_SIGNAL_NONE;
  ASSERT_OK(
      root.client.channel().wait_one(ZX_CHANNEL_PEER_CLOSED, zx::time::infinite(), &observed));
  ASSERT_TRUE(observed & ZX_CHANNEL_PEER_CLOSED);

  loop().Shutdown();
}

}  // namespace
