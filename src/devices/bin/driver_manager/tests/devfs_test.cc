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

#include "src/devices/bin/driver_manager/devfs/devfs_exporter.h"

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

  ASSERT_OK(root_node.export_dir(Devnode::Target(Devnode::Connector{}), "one/two", {}, out));

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

  ASSERT_STATUS(root_node.export_dir(Devnode::Target(Devnode::Connector{}), "one//two", {}, out),
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

  ASSERT_OK(root_node.export_dir(Devnode::Target(Devnode::Connector{}), "one", {}, out));
  std::optional node_one = lookup(root_node, "one");
  ASSERT_TRUE(node_one.has_value());
  EXPECT_EQ("one", node_one->get().name());

  ASSERT_OK(root_node.export_dir(Devnode::Target(Devnode::Connector{}), "one/two", {}, out));
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
                root_node.export_dir(Devnode::Target(Devnode::Connector{}), "", {}, out));
  ASSERT_STATUS(ZX_ERR_INVALID_ARGS,
                root_node.export_dir(Devnode::Target(Devnode::Connector{}), "/one/two", {}, out));
  ASSERT_STATUS(ZX_ERR_INVALID_ARGS,
                root_node.export_dir(Devnode::Target(Devnode::Connector{}), "one/two/", {}, out));
  ASSERT_STATUS(ZX_ERR_INVALID_ARGS,
                root_node.export_dir(Devnode::Target(Devnode::Connector{}), "/one/two/", {}, out));
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
  ASSERT_OK(root_node.export_dir(Devnode::Target(Devnode::Connector{}), "one/two", "block", out));

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

  ASSERT_OK(root_node.export_dir(Devnode::Target(Devnode::Connector{}), "one/two", {}, out));
  ASSERT_STATUS(ZX_ERR_ALREADY_EXISTS,
                root_node.export_dir(Devnode::Target(Devnode::Connector{}), "one/two", {}, out));
}

TEST(Devfs, Export_DropDevfs) {
  std::optional<Devnode> root_slot;
  const Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode& root_node = root_slot.value();
  std::vector<std::unique_ptr<Devnode>> out;

  ASSERT_OK(root_node.export_dir(Devnode::Target(Devnode::Connector{}), "one/two", {}, out));

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

TEST(Devfs, ExportWatcherConnector_Export) {
  async::Loop loop{&kAsyncLoopConfigNoAttachToCurrentThread};

  std::optional<Devnode> root_slot;
  Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode& root_node = root_slot.value();

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_device_fs::Connector>();
  ASSERT_OK(endpoints);

  zx::result export_watcher = driver_manager::ExportWatcher::Create(
      loop.dispatcher(), devfs, &root_node, std::move(endpoints->client), "one/two", "block",
      fuchsia_device_fs::wire::ExportOptions());
  ASSERT_OK(export_watcher);

  // Set our ExportWatcher to let us know if the service was closed.
  bool did_close = false;
  export_watcher.value()->set_on_close_callback(
      [&did_close](driver_manager::ExportWatcher*) { did_close = true; });

  // Make sure the directories were set up correctly.
  {
    std::optional node_one = lookup(root_node, "one");
    ASSERT_TRUE(node_one.has_value());
    EXPECT_EQ("one", node_one->get().name());

    std::optional node_two = lookup(node_one->get(), "two");
    ASSERT_TRUE(node_two.has_value());
    EXPECT_EQ("two", node_two->get().name());
  }

  // Close our channel and check that ExportWatcher called the callback.
  endpoints->server.Close(ZX_OK);
  loop.RunUntilIdle();
  ASSERT_TRUE(did_close);
  // We didn't tear down in the callback so this should still exist.
  ASSERT_TRUE(lookup(root_node, "one").has_value());

  // Drop ExportWatcher and make sure the devfs nodes disappeared.
  export_watcher.value().reset();
  ASSERT_FALSE(lookup(root_node, "one").has_value());
}

TEST(Devfs, ExportWatcherConnector_BadClass) {
  async::Loop loop{&kAsyncLoopConfigNoAttachToCurrentThread};

  std::optional<Devnode> root_slot;
  Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode& root_node = root_slot.value();

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_device_fs::Connector>();
  ASSERT_OK(endpoints.status_value());

  zx::result export_watcher = driver_manager::ExportWatcher::Create(
      loop.dispatcher(), devfs, &root_node, std::move(endpoints->client), std::nullopt,
      "NOT_REAL_CLASS", fuchsia_device_fs::wire::ExportOptions());
  ASSERT_STATUS(ZX_ERR_NOT_FOUND, export_watcher.status_value());
}

TEST(Devfs, ExportWatcherConnector_TopologicalPathExists) {
  async::Loop loop{&kAsyncLoopConfigNoAttachToCurrentThread};

  std::optional<Devnode> root_slot;
  Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode& root_node = root_slot.value();

  auto create_watcher = [&devfs, &root_node,
                         &loop]() -> zx::result<std::unique_ptr<driver_manager::ExportWatcher>> {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_device_fs::Connector>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }

    return driver_manager::ExportWatcher::Create(
        loop.dispatcher(), devfs, &root_node, std::move(endpoints->client), "one/two", std::nullopt,
        fuchsia_device_fs::wire::ExportOptions());
  };

  zx::result watcher1 = create_watcher();
  ASSERT_OK(watcher1);

  zx::result watcher2 = create_watcher();
  ASSERT_STATUS(ZX_ERR_ALREADY_EXISTS, watcher2.status_value());
}

TEST(Devfs, ExportWatcherConnector_Export_Invisible) {
  async::Loop loop{&kAsyncLoopConfigNoAttachToCurrentThread};

  std::optional<Devnode> root_slot;
  Devfs devfs(root_slot);
  ASSERT_TRUE(root_slot.has_value());
  Devnode& root_node = root_slot.value();

  // Create the export server and client.
  driver_manager::DevfsExporter exporter{devfs, &root_node, loop.dispatcher()};
  fidl::WireClient<fuchsia_device_fs::Exporter> exporter_client;
  {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_device_fs::Exporter>();
    ASSERT_OK(endpoints);
    fidl::BindServer(loop.dispatcher(), std::move(endpoints->server), &exporter);
    exporter_client.Bind(std::move(endpoints->client), loop.dispatcher());
  }

  Connecter connecter;
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_device_fs::Connector>();
  ASSERT_OK(endpoints);

  fidl::ServerBindingRef binding_ref =
      fidl::BindServer(loop.dispatcher(), std::move(endpoints->server), &connecter);

  exporter_client
      ->ExportV2(std::move(endpoints->client), "one/two", "block",
                 fuchsia_device_fs::wire::ExportOptions::kInvisible)
      .Then([](auto& result) { ASSERT_OK(result.status()); });
  ASSERT_OK(loop.RunUntilIdle());

  {
    std::optional node_one = lookup(root_node, "one");
    ASSERT_TRUE(node_one.has_value());
    EXPECT_EQ("one", node_one->get().name());
    EXPECT_EQ(fuchsia_device_fs::wire::ExportOptions::kInvisible, node_one->get().export_options());

    std::optional node_two = lookup(node_one.value(), "two");
    ASSERT_TRUE(node_two.has_value());
    EXPECT_EQ("two", node_two->get().name());
    EXPECT_EQ(fuchsia_device_fs::wire::ExportOptions::kInvisible, node_two->get().export_options());
  }

  // Try and make a subdir visible, this will fail because the devfs path has to match exactly with
  // Export.
  exporter_client->MakeVisible("one").Then(
      [](fidl::WireUnownedResult<fuchsia_device_fs::Exporter::MakeVisible>& result) {
        ASSERT_OK(result.status());
        ASSERT_FALSE(result->is_ok());
        ASSERT_STATUS(ZX_ERR_NOT_FOUND, result->error_value());
      });

  exporter_client->MakeVisible("one/two").Then([](auto& result) {
    ASSERT_OK(result.status());
    ASSERT_TRUE(result->is_ok(), "MakeVisible Failed %s",
                zx_status_get_string(result->error_value()));
  });
  ASSERT_OK(loop.RunUntilIdle());

  {
    std::optional node_one = lookup(root_node, "one");
    ASSERT_TRUE(node_one.has_value());
    EXPECT_EQ("one", node_one->get().name());
    EXPECT_EQ(fuchsia_device_fs::wire::ExportOptions(), node_one->get().export_options());

    std::optional node_two = lookup(node_one->get(), "two");
    ASSERT_TRUE(node_two.has_value());
    EXPECT_EQ("two", node_two->get().name());
    EXPECT_EQ(fuchsia_device_fs::wire::ExportOptions(), node_two->get().export_options());
  }

  exporter_client->MakeVisible("one/two").Then([](auto& result) {
    ASSERT_OK(result.status());
    ASSERT_FALSE(result->is_ok());
    ASSERT_STATUS(ZX_ERR_BAD_STATE, result->error_value());
  });

  ASSERT_OK(loop.RunUntilIdle());
}

}  // namespace
