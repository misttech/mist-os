// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>

#include <span>
#include <utility>

#include <gtest/gtest.h>

#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/service.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"

namespace {

namespace fio = fuchsia_io;

TEST(Service, ConstructWithRawChannelConnector) {
  auto svc = fbl::MakeRefCounted<fs::Service>([](zx::channel channel) { return ZX_OK; });
}

TEST(Service, ConstructWithTypedChannelConnector) {
  auto svc = fbl::MakeRefCounted<fs::Service>(
      [](fidl::ServerEnd<fio::Directory> server_end) { return ZX_OK; });
}

TEST(Service, ApiTest) {
  // Set up a service which can only be bound once (to make it easy to simulate an error to test
  // error reporting behavior from the connector)
  zx::channel bound_channel;
  auto svc = fbl::MakeRefCounted<fs::Service>([&bound_channel](zx::channel channel) {
    if (bound_channel)
      return ZX_ERR_IO;
    bound_channel = std::move(channel);
    return ZX_OK;
  });

  // open
  fbl::RefPtr<fs::Vnode> redirect;
  auto result = svc->ValidateOptions({});
  ASSERT_TRUE(result.is_ok());
  ASSERT_EQ(svc->Open(&redirect), ZX_OK);
  EXPECT_TRUE(!redirect);

  // protocols and attributes
  EXPECT_EQ(fuchsia_io::NodeProtocolKinds::kConnector, svc->GetProtocols());
  zx::result attr = svc->GetAttributes();
  ASSERT_TRUE(attr.is_ok());
  EXPECT_EQ(V_TYPE_FILE, attr->mode);

  // make some channels we can use for testing
  zx::channel c1, c2;
  ASSERT_EQ(zx::channel::create(0u, &c1, &c2), ZX_OK);
  zx_handle_t hc1 = c1.get();

  // serve, the connector will return success the first time
  fs::SynchronousVfs vfs;
  ASSERT_EQ(vfs.Serve(svc, std::move(c1), fio::Flags::kProtocolService), ZX_OK);
  EXPECT_EQ(hc1, bound_channel.get());

  // The connector will return failure because bound_channel is still valid we test that the error
  // is propagated back up through Serve,
  ASSERT_EQ(ZX_ERR_IO, vfs.Serve(svc, std::move(c2), fio::Flags::kProtocolService));
  EXPECT_EQ(hc1, bound_channel.get());
}

TEST(Service, ServeDirectory) {
  auto root = fidl::Endpoints<fio::Directory>::Create();

  // open client
  zx::channel c1, c2;
  ASSERT_EQ(zx::channel::create(0u, &c1, &c2), ZX_OK);
  ASSERT_EQ(fdio_service_connect_at(root.client.borrow().channel()->get(), "abc", c2.release()),
            ZX_OK);

  // Close client. We test the semantic that a pending open is processed even if the client has been
  // closed.
  root.client.reset();

  // serve
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  fs::SynchronousVfs vfs(loop.dispatcher());

  auto directory = fbl::MakeRefCounted<fs::PseudoDir>();
  auto vnode = fbl::MakeRefCounted<fs::Service>([&loop](zx::channel channel) {
    loop.Shutdown();
    return ZX_OK;
  });
  directory->AddEntry("abc", vnode);

  ASSERT_EQ(vfs.ServeDirectory(directory, std::move(root.server)), ZX_OK);
  EXPECT_EQ(ZX_ERR_BAD_STATE, loop.RunUntilIdle());
}

TEST(Service, ServiceNodeIsNotDirectory) {
  // Set up the server
  auto [root_client, root_server] = fidl::Endpoints<fio::Directory>::Create();

  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  fs::SynchronousVfs vfs(loop.dispatcher());

  auto directory = fbl::MakeRefCounted<fs::PseudoDir>();
  auto vnode = fbl::MakeRefCounted<fs::Service>([](zx::channel channel) {
    // Should never reach here, because the directory flag is not allowed.
    EXPECT_TRUE(false) << "Should not be able to open the service";
    channel.reset();
    return ZX_OK;
  });
  directory->AddEntry("abc", vnode);
  ASSERT_EQ(vfs.ServeDirectory(directory, std::move(root_server)), ZX_OK);

  // Call |ValidateOptions| with the directory flag should fail.
  auto result = vnode->ValidateOptions(fs::VnodeConnectionOptions{
      .flags = fuchsia_io::OpenFlags::kDirectory,
      .rights = fuchsia_io::kRwStarDir,
  });
  ASSERT_TRUE(result.is_error());
  ASSERT_EQ(ZX_ERR_NOT_DIR, result.status_value());

  // Open the service through FIDL with the directory flag, which should fail.
  auto [client, server] = fidl::Endpoints<fio::Node>::Create();
  loop.StartThread();

  auto open_result =
      fidl::WireCall(root_client)
          ->Open(fidl::StringView("abc"),
                 fio::wire::Flags::kFlagSendRepresentation | fio::wire::Flags::kProtocolDirectory |
                     fio::wire::kPermReadable | fio::wire::kPermWritable,
                 {}, server.TakeChannel());
  // Request should succeed but `client` should be closed with epitaph.
  ASSERT_EQ(open_result.status(), ZX_OK);
  class EventHandler : public fidl::testing::WireSyncEventHandlerTestBase<fio::Node> {
   public:
    EventHandler() = default;

    void NotImplemented_(const std::string& name) override {
      ADD_FAILURE() << "Unexpected " << name;
    }
  };

  EventHandler event_handler;
  fidl::Status handler_result = event_handler.HandleOneEvent(client);
  ASSERT_TRUE(handler_result.is_peer_closed());
  ASSERT_EQ(handler_result.status(), ZX_ERR_NOT_DIR);

  loop.Shutdown();
}

TEST(Service, OpeningServiceWithNodeProtocol) {
  // Set up the server
  auto [root_client, root_server] = fidl::Endpoints<fio::Directory>::Create();

  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  fs::SynchronousVfs vfs(loop.dispatcher());

  auto directory = fbl::MakeRefCounted<fs::PseudoDir>();
  auto vnode = fbl::MakeRefCounted<fs::Service>([](zx::channel channel) {
    channel.reset();
    return ZX_OK;
  });
  directory->AddEntry("abc", vnode);
  ASSERT_EQ(vfs.ServeDirectory(directory, std::move(root_server)), ZX_OK);

  auto [client, server] = fidl::Endpoints<fio::Node>::Create();

  loop.StartThread();

  ASSERT_EQ(
      fidl::WireCall(root_client)
          ->Open(fidl::StringView("abc"), fio::wire::Flags::kProtocolNode, {}, server.TakeChannel())
          .status(),
      ZX_OK);

  // The channel should speak |fuchsia.io/Node| instead of the custom service FIDL protocol.
  const fidl::WireResult result = fidl::WireCall(client)->Query();
  ASSERT_EQ(result.status(), ZX_OK);
  const fidl::WireResponse response = result.value();
  const std::span data = response.protocol.get();
  const std::string_view protocol{reinterpret_cast<const char*>(data.data()), data.size_bytes()};
  ASSERT_EQ(protocol, fio::wire::kNodeProtocolName);

  loop.Shutdown();
}

}  // namespace
