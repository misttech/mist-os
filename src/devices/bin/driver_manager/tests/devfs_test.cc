// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/devfs/devfs.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/driver.h>

#include <functional>

#include <zxtest/zxtest.h>

#include "src/lib/storage/vfs/cpp/synchronous_vfs.h"

namespace {

namespace fio = fuchsia_io;

class Connecter : public fidl::WireServer<fuchsia_device_fs::Connector> {
 public:
 private:
  void Connect(ConnectRequestView request, ConnectCompleter::Sync& completer) override {
    ASSERT_EQ(channel_.get(), ZX_HANDLE_INVALID);
    channel_ = std::move(request->server);
  }

  zx::channel channel_;
};

std::optional<std::reference_wrapper<const Devnode>> lookup(const Devnode& parent,
                                                            std::string_view name) {
  {
    fbl::RefPtr<fs::Vnode> out;
    switch (const zx_status_t status = parent.children().Lookup(name, &out); status) {
      case ZX_OK:
        return std::reference_wrapper(fbl::RefPtr<Devnode::VnodeImpl>::Downcast(out)->holder_);
      case ZX_ERR_NOT_FOUND:
        break;
      default:
        ADD_FAILURE("%s", zx_status_get_string(status));
        return {};
    }
  }
  const auto it = parent.children().unpublished.find(name);
  if (it != parent.children().unpublished.end()) {
    return it->second.get();
  }
  return {};
}

TEST(Devfs, Export) {
  std::optional<Devnode> root_slot;
  const Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode& root_node = root_slot.value();
  std::vector<std::unique_ptr<Devnode>> out;

  ASSERT_OK(root_node.export_dir(Devnode::Target(Devnode::NoRemote{}), "one/two", {}, out));

  std::optional node_one = lookup(root_node, "one");
  ASSERT_TRUE(node_one.has_value());
  EXPECT_EQ("one", node_one->get().name());
  std::optional node_two = lookup(node_one->get(), "two");
  ASSERT_TRUE(node_two.has_value());
  EXPECT_EQ("two", node_two->get().name());
}

TEST(Devfs, Export_ExcessSeparators) {
  std::optional<Devnode> root_slot;
  const Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode& root_node = root_slot.value();
  std::vector<std::unique_ptr<Devnode>> out;

  ASSERT_STATUS(root_node.export_dir(Devnode::Target(Devnode::NoRemote{}), "one//two", {}, out),
                ZX_ERR_INVALID_ARGS);

  ASSERT_FALSE(lookup(root_node, "one").has_value());
  ASSERT_FALSE(lookup(root_node, "two").has_value());
}

TEST(Devfs, Export_OneByOne) {
  std::optional<Devnode> root_slot;
  const Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode& root_node = root_slot.value();
  std::vector<std::unique_ptr<Devnode>> out;

  ASSERT_OK(root_node.export_dir(Devnode::Target(Devnode::NoRemote{}), "one", {}, out));
  std::optional node_one = lookup(root_node, "one");
  ASSERT_TRUE(node_one.has_value());
  EXPECT_EQ("one", node_one->get().name());

  ASSERT_OK(root_node.export_dir(Devnode::Target(Devnode::NoRemote{}), "one/two", {}, out));
  std::optional node_two = lookup(node_one->get(), "two");
  ASSERT_TRUE(node_two.has_value());
  EXPECT_EQ("two", node_two->get().name());
}

TEST(Devfs, Export_InvalidPath) {
  std::optional<Devnode> root_slot;
  const Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode& root_node = root_slot.value();
  std::vector<std::unique_ptr<Devnode>> out;

  ASSERT_STATUS(ZX_ERR_INVALID_ARGS,
                root_node.export_dir(Devnode::Target(Devnode::NoRemote{}), "", {}, out));
  ASSERT_STATUS(ZX_ERR_INVALID_ARGS,
                root_node.export_dir(Devnode::Target(Devnode::NoRemote{}), "/one/two", {}, out));
  ASSERT_STATUS(ZX_ERR_INVALID_ARGS,
                root_node.export_dir(Devnode::Target(Devnode::NoRemote{}), "one/two/", {}, out));
  ASSERT_STATUS(ZX_ERR_INVALID_ARGS,
                root_node.export_dir(Devnode::Target(Devnode::NoRemote{}), "/one/two/", {}, out));
}

TEST(Devfs, Export_WithProtocol) {
  std::optional<Devnode> root_slot;
  Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode& root_node = root_slot.value();

  std::optional proto_node = devfs.proto_node(ZX_PROTOCOL_BLOCK);
  ASSERT_TRUE(proto_node.has_value());
  EXPECT_EQ("block", proto_node.value().get().name());
  {
    fbl::RefPtr<fs::Vnode> node_000;
    EXPECT_STATUS(proto_node.value().get().children().Lookup("000", &node_000), ZX_ERR_NOT_FOUND);
    ASSERT_EQ(node_000, nullptr);
  }

  std::vector<std::unique_ptr<Devnode>> out;
  ASSERT_OK(root_node.export_dir(Devnode::Target(Devnode::NoRemote{}), "one/two", "block", out));

  std::optional node_one = lookup(root_node, "one");
  ASSERT_TRUE(node_one.has_value());
  EXPECT_EQ("one", node_one->get().name());

  std::optional node_two = lookup(node_one->get(), "two");
  ASSERT_TRUE(node_two.has_value());
  EXPECT_EQ("two", node_two->get().name());

  fbl::RefPtr<fs::Vnode> node_000;
  ASSERT_OK(proto_node.value().get().children().Lookup("000", &node_000));
  ASSERT_NE(node_000, nullptr);
}

TEST(Devfs, Export_AlreadyExists) {
  std::optional<Devnode> root_slot;
  const Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode& root_node = root_slot.value();
  std::vector<std::unique_ptr<Devnode>> out;

  ASSERT_OK(root_node.export_dir(Devnode::Target(Devnode::NoRemote{}), "one/two", {}, out));
  ASSERT_STATUS(ZX_ERR_ALREADY_EXISTS,
                root_node.export_dir(Devnode::Target(Devnode::NoRemote{}), "one/two", {}, out));
}

TEST(Devfs, Export_DropDevfs) {
  std::optional<Devnode> root_slot;
  const Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode& root_node = root_slot.value();
  std::vector<std::unique_ptr<Devnode>> out;

  ASSERT_OK(root_node.export_dir(Devnode::Target(Devnode::NoRemote{}), "one/two", {}, out));

  {
    std::optional node_one = lookup(root_node, "one");
    ASSERT_TRUE(node_one.has_value());
    EXPECT_EQ("one", node_one->get().name());

    std::optional node_two = lookup(node_one->get(), "two");
    ASSERT_TRUE(node_two.has_value());
    EXPECT_EQ("two", node_two->get().name());
  }

  out.clear();

  ASSERT_FALSE(lookup(root_node, "one").has_value());
}

TEST(Devfs, PassthroughTarget) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  fs::SynchronousVfs vfs(loop.dispatcher());

  std::optional<Devnode> root_slot;
  Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode::PassThrough::ConnectionType connection_type;
  Devnode::PassThrough passthrough({
      [&loop, &connection_type](zx::channel server, Devnode::PassThrough::ConnectionType type) {
        connection_type = type;
        loop.Quit();
        return ZX_OK;
      },
  });

  DevfsDevice device;
  ASSERT_OK(root_slot.value().add_child("test", std::nullopt, passthrough.Clone(), device));
  device.publish();

  zx::result devfs_client = devfs.Connect(vfs);
  ASSERT_OK(devfs_client);

  struct TestRun {
    const char* file_name;
    Devnode::PassThrough::ConnectionType expected;
  };

  const TestRun tests[] = {
      {
          .file_name = "test",
          .expected =
              {
                  .include_node = true,
                  .include_controller = true,
                  .include_device = true,
              },
      },
      {
          .file_name = "test/device_controller",
          .expected =
              {
                  .include_node = false,
                  .include_controller = true,
                  .include_device = false,
              },
      },
      {
          .file_name = "test/device_protocol",
          .expected =
              {
                  .include_node = false,
                  .include_controller = false,
                  .include_device = true,
              },
      },
  };

  for (const TestRun& test : tests) {
    SCOPED_TRACE(test.file_name);
    zx::result file_endpoints = fidl::CreateEndpoints<fuchsia_io::Node>();
    ASSERT_OK(file_endpoints);

    ASSERT_OK(fidl::WireCall(devfs_client.value())
                  ->Open(fuchsia_io::wire::OpenFlags(), fuchsia_io::wire::ModeType(),
                         fidl::StringView::FromExternal(test.file_name),
                         std::move(file_endpoints->server))
                  .status());
    loop.Run();
    loop.ResetQuit();

    ASSERT_EQ(connection_type.include_device, test.expected.include_device);
    ASSERT_EQ(connection_type.include_controller, test.expected.include_controller);
    ASSERT_EQ(connection_type.include_node, test.expected.include_node);
  }
}

}  // namespace
