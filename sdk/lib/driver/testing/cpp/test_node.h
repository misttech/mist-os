// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_TEST_NODE_H_
#define LIB_DRIVER_TESTING_CPP_TEST_NODE_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>

namespace fdf_testing {

class TestNode : public fidl::WireServer<fuchsia_driver_framework::NodeController>,
                 public fidl::WireServer<fuchsia_driver_framework::Node> {
 public:
  using ChildrenMap = std::unordered_map<std::string, TestNode>;
  struct BindData {
    bool force_rebind;
    std::string driver_url_suffix;
  };

  struct CreateStartArgsResult {
    fuchsia_driver_framework::DriverStartArgs start_args;
    fidl::ServerEnd<fuchsia_io::Directory> incoming_directory_server;
    fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client;
  };

  // If no dispatcher is provided, this will try to use the current fdf_dispatcher. If there
  // is also no fdf_dispatcher, it will try to use the thread's default async_dispatcher.
  explicit TestNode(std::string name, async_dispatcher_t* dispatcher = nullptr);

  ~TestNode() override;

  ChildrenMap& children() { return children_; }
  const std::string& name() const { return name_; }

  // Create a channel pair, serve the server end, and return the client end.
  // This method is thread-unsafe. Must be called from the same context as the dispatcher.
  zx::result<fidl::ClientEnd<fuchsia_driver_framework::Node>> CreateNodeChannel();

  // Serve the given server end.
  // This method is thread-unsafe. Must be called from the same context as the dispatcher.
  zx::result<> Serve(fidl::ServerEnd<fuchsia_driver_framework::Node> server_end);

  // Creates the start args for a driver, and serve the fdf::Node in it.
  zx::result<CreateStartArgsResult> CreateStartArgsAndServe();

  // Connects to the devfs device this node is serving.
  zx::result<zx::channel> ConnectToDevice();

  bool HasNode() {
    std::lock_guard guard(checker_);
    return node_binding_.has_value();
  }

  std::vector<fuchsia_driver_framework::NodeProperty> GetProperties() const {
    std::lock_guard guard(checker_);
    return properties_;
  }
  std::vector<BindData> GetBindData() const {
    std::lock_guard guard(checker_);
    return bind_data_;
  }

  async_dispatcher_t* dispatcher() const { return dispatcher_; }

 private:
  void AddChild(AddChildRequestView request, AddChildCompleter::Sync& completer) override;

  void Remove(RemoveCompleter::Sync& completer) override { RemoveFromParent(); }
  void RequestBind(RequestBindRequestView request, RequestBindCompleter::Sync& completer) override;

  void SetParent(TestNode* parent,
                 fidl::ServerEnd<fuchsia_driver_framework::NodeController> controller);
  void SetProperties(std::vector<fuchsia_driver_framework::NodeProperty> properties);

  void RemoveFromParent();

  void RemoveChild(const std::string& name);

  void set_devfs_connector_client(fidl::ClientEnd<fuchsia_device_fs::Connector> client) {
    std::lock_guard guard(checker_);
    devfs_connector_client_.Bind(std::move(client), dispatcher_);
  }

  std::optional<fidl::ServerBinding<fuchsia_driver_framework::Node>> node_binding_
      __TA_GUARDED(checker_);
  std::optional<fidl::ServerBinding<fuchsia_driver_framework::NodeController>> controller_binding_
      __TA_GUARDED(checker_);

  async_dispatcher_t* dispatcher_;
  std::vector<fuchsia_driver_framework::NodeProperty> properties_ __TA_GUARDED(checker_);
  std::vector<BindData> bind_data_ __TA_GUARDED(checker_);
  std::string name_ __TA_GUARDED(checker_);
  std::optional<std::reference_wrapper<TestNode>> parent_ __TA_GUARDED(checker_);
  ChildrenMap children_ __TA_GUARDED(checker_);
  fidl::WireClient<fuchsia_device_fs::Connector> devfs_connector_client_ __TA_GUARDED(checker_);
  async::synchronization_checker checker_;
};

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_TEST_NODE_H_
