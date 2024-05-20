// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_TESTING_VISITOR_TEST_HELPER_H_
#define LIB_DRIVER_DEVICETREE_TESTING_VISITOR_TEST_HELPER_H_

#include <lib/driver/devicetree/manager/manager-test-helper.h>

namespace fdf_devicetree::testing {

template <class VisitorImpl>
class VisitorTestHelper : public VisitorImpl, public ManagerTestHelper {
  static_assert(std::is_base_of_v<Visitor, VisitorImpl>, "VisitorImpl has to inherit from Visitor");

 public:
  VisitorTestHelper(std::string_view dtb_path, std::string_view log_tag)
      : ManagerTestHelper(log_tag), dtb_path_(dtb_path) {}

  zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder) override {
    visit_called_ = true;
    return VisitorImpl::Visit(node, decoder);
  }

  bool has_visited() { return visit_called_; }

  zx::result<> DoPublish() { return ManagerTestHelper::DoPublish(*manager_); }

  Manager* manager() {
    if (!manager_) {
      manager_ = std::make_unique<Manager>(LoadTestBlob(dtb_path_.data()));
    }
    return manager_.get();
  }

  std::vector<std::shared_ptr<fuchsia_driver_framework::NodeAddChildRequest>> GetNodes(
      const std::string& name_filter) {
    std::vector<std::shared_ptr<fuchsia_driver_framework::NodeAddChildRequest>> output_nodes;
    auto node_count = env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::non_pbus_node_size);
    for (size_t i = 0; i < node_count; i++) {
      auto node = env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::non_pbus_nodes_at, i);
      if (node->args().name()->find(name_filter) != std::string::npos) {
        output_nodes.push_back(node);
      }
    }
    return output_nodes;
  }

  std::vector<fuchsia_hardware_platform_bus::Node> GetPbusNodes(const std::string& name_filter) {
    std::vector<fuchsia_hardware_platform_bus::Node> output_nodes;
    auto node_count = env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);
    for (size_t i = 0; i < node_count; i++) {
      auto node = env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);
      if (node.name()->find(name_filter) != std::string::npos) {
        output_nodes.push_back(node);
      }
    }
    return output_nodes;
  }

  std::vector<fuchsia_driver_framework::CompositeNodeSpec> GetCompositeNodeSpecs(
      const std::string& name_filter) {
    std::vector<fuchsia_driver_framework::CompositeNodeSpec> output_specs;
    auto request_count =
        env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size);
    for (size_t i = 0; i < request_count; i++) {
      auto request = env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, i);
      if (request.name()->find(name_filter) != std::string::npos) {
        output_specs.push_back(request);
      }
    }
    return output_specs;
  }

 private:
  bool visit_called_ = false;
  std::unique_ptr<Manager> manager_;
  std::string_view dtb_path_;
};

}  // namespace fdf_devicetree::testing

#endif  // LIB_DRIVER_DEVICETREE_TESTING_VISITOR_TEST_HELPER_H_
