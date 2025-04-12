// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../mailbox-visitor.h"

#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/default/mmio/mmio.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <optional>
#include <string_view>

#include <bind/fuchsia/hardware/mailbox/cpp/bind.h>
#include <bind/fuchsia/mailbox/cpp/bind.h>
#include <gtest/gtest.h>

namespace {

std::optional<fuchsia_hardware_mailbox::ChannelInfo> FindChannel(
    uint32_t channel, const fuchsia_hardware_mailbox::ControllerInfo& controller_info) {
  auto it =
      std::find_if(controller_info.channels()->cbegin(), controller_info.channels()->cend(),
                   [channel](const fuchsia_hardware_mailbox::ChannelInfo& channel_info) -> bool {
                     return channel_info.channel() && channel_info.channel() == channel;
                   });
  if (it == controller_info.channels()->end()) {
    return std::nullopt;
  }
  return {*it};
}

}  // namespace

namespace mailbox_dt {

class MailboxVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<MailboxVisitor> {
 public:
  explicit MailboxVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<MailboxVisitor>(dtb_path,
                                                                   "MailboxBusVisitorTest") {}

  std::optional<fidl::Request<fuchsia_driver_framework::CompositeNodeManager::AddSpec>>
  FindMgrRequest(std::string_view name) {
    const size_t mgr_request_size =
        env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size);
    for (size_t i = 0; i < mgr_request_size; i++) {
      fidl::Request<fuchsia_driver_framework::CompositeNodeManager::AddSpec> request =
          env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, i);
      if (request.name() && *request.name() == name) {
        return request;
      }
    }

    return {};
  }

  std::optional<fuchsia_hardware_platform_bus::Node> FindPbusNode(std::string_view name) {
    const size_t pbus_node_size =
        env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);
    for (size_t i = 0; i < pbus_node_size; i++) {
      fuchsia_hardware_platform_bus::Node node =
          env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);
      if (node.name() && *node.name() == name) {
        return node;
      }
    }

    return {};
  }

  std::optional<const fdf_devicetree::Node*> FindDevicetreeNode(std::string_view name) {
    for (auto& node : manager()->nodes()) {
      if (node->name() == name) {
        return node.get();
      }
    }
    return {};
  }
};

TEST(MailboxVisitorTest, TwoControllers) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<fdf_devicetree::MmioVisitor>()).is_ok());

  MailboxVisitorTester* const mailbox_tester =
      new MailboxVisitorTester("/pkg/test-data/mailbox.dtb");
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::unique_ptr<MailboxVisitorTester>{mailbox_tester}).is_ok());

  ASSERT_TRUE(mailbox_tester->manager()->Walk(visitors).is_ok());
  ASSERT_TRUE(mailbox_tester->DoPublish().is_ok());

  // First controller metadata
  const auto pbus_node_0 = mailbox_tester->FindPbusNode("mailbox-abcd0000");
  ASSERT_TRUE(pbus_node_0);

  ASSERT_TRUE(pbus_node_0->metadata());
  ASSERT_EQ(pbus_node_0->metadata()->size(), 1u);

  ASSERT_TRUE((*pbus_node_0->metadata())[0].id());
  EXPECT_EQ(*(*pbus_node_0->metadata())[0].id(),
            std::to_string(fuchsia_hardware_mailbox::kControllerInfoMetadataType));

  ASSERT_TRUE((*pbus_node_0->metadata())[0].data());
  const std::vector<uint8_t>& metadata_0 = *(*pbus_node_0->metadata())[0].data();

  const auto controller_0 = fidl::Unpersist<fuchsia_hardware_mailbox::ControllerInfo>(
      {metadata_0.data(), metadata_0.size()});
  ASSERT_TRUE(controller_0.is_ok());

  ASSERT_TRUE(controller_0->id());
  const uint32_t controller_0_id = *controller_0->id();

  ASSERT_TRUE(controller_0->channels());
  ASSERT_EQ(controller_0->channels()->size(), 2u);

  EXPECT_TRUE(FindChannel(0x1234, *controller_0));
  EXPECT_TRUE(FindChannel(0x5678, *controller_0));

  // Second controller metadata
  const auto pbus_node_1 = mailbox_tester->FindPbusNode("mailbox-abce0000");
  ASSERT_TRUE(pbus_node_1);

  ASSERT_TRUE(pbus_node_1->metadata());
  ASSERT_EQ(pbus_node_1->metadata()->size(), 1u);

  ASSERT_TRUE((*pbus_node_1->metadata())[0].id());
  EXPECT_EQ(*(*pbus_node_1->metadata())[0].id(),
            std::to_string(fuchsia_hardware_mailbox::kControllerInfoMetadataType));

  ASSERT_TRUE((*pbus_node_1->metadata())[0].data());
  const std::vector<uint8_t>& metadata_1 = *(*pbus_node_1->metadata())[0].data();

  const auto controller_1 = fidl::Unpersist<fuchsia_hardware_mailbox::ControllerInfo>(
      {metadata_1.data(), metadata_1.size()});
  ASSERT_TRUE(controller_1.is_ok());

  ASSERT_TRUE(controller_1->id());
  const uint32_t controller_1_id = *controller_1->id();

  ASSERT_TRUE(controller_1->channels());
  ASSERT_EQ(controller_1->channels()->size(), 2u);

  EXPECT_TRUE(FindChannel(0x9abc, *controller_1));
  EXPECT_TRUE(FindChannel(0x1234, *controller_1));

  // First client composite node specs
  const auto client_0 = mailbox_tester->FindMgrRequest("node-abcf0000_group");
  ASSERT_TRUE(client_0);

  ASSERT_TRUE(client_0->parents());
  ASSERT_EQ(client_0->parents()->size(), 4u);

  // The 0th composite parent has the `compatible` string and is added by the default visitor.
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_mailbox::SERVICE,
                                  bind_fuchsia_hardware_mailbox::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia_mailbox::CONTROLLER_ID, controller_0_id),
          fdf::MakeAcceptBindRule(bind_fuchsia_mailbox::CHANNEL, 0x1234u),
      },
      (*client_0->parents())[1].bind_rules(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {
          fdf::MakeProperty(bind_fuchsia_hardware_mailbox::SERVICE,
                            bind_fuchsia_hardware_mailbox::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia_mailbox::CONTROLLER_ID, 0u),
          fdf::MakeProperty(bind_fuchsia_mailbox::CHANNEL, 0x1234u),
          fdf::MakeProperty(bind_fuchsia_mailbox::CHANNEL_NAME, "mailbox-1-1234"),
      },
      (*client_0->parents())[1].properties(), false));

  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_mailbox::SERVICE,
                                  bind_fuchsia_hardware_mailbox::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia_mailbox::CONTROLLER_ID, controller_0_id),
          fdf::MakeAcceptBindRule(bind_fuchsia_mailbox::CHANNEL, 0x5678u),
      },
      (*client_0->parents())[2].bind_rules(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {
          fdf::MakeProperty(bind_fuchsia_hardware_mailbox::SERVICE,
                            bind_fuchsia_hardware_mailbox::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia_mailbox::CONTROLLER_ID, 0u),
          fdf::MakeProperty(bind_fuchsia_mailbox::CHANNEL, 0x5678u),
          fdf::MakeProperty(bind_fuchsia_mailbox::CHANNEL_NAME, "mailbox-1-5678"),
      },
      (*client_0->parents())[2].properties(), false));

  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_mailbox::SERVICE,
                                  bind_fuchsia_hardware_mailbox::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia_mailbox::CONTROLLER_ID, controller_1_id),
          fdf::MakeAcceptBindRule(bind_fuchsia_mailbox::CHANNEL, 0x9abcu),
      },
      (*client_0->parents())[3].bind_rules(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {
          fdf::MakeProperty(bind_fuchsia_hardware_mailbox::SERVICE,
                            bind_fuchsia_hardware_mailbox::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia_mailbox::CONTROLLER_ID, 1u),
          fdf::MakeProperty(bind_fuchsia_mailbox::CHANNEL, 0x9abcu),
          fdf::MakeProperty(bind_fuchsia_mailbox::CHANNEL_NAME, "mailbox-2-9abc"),
      },
      (*client_0->parents())[3].properties(), false));

  // Second client composite node specs
  const auto client_1 = mailbox_tester->FindMgrRequest("node-abd00000_group");
  ASSERT_TRUE(client_1);

  ASSERT_TRUE(client_1->parents());
  ASSERT_EQ(client_1->parents()->size(), 2u);

  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_mailbox::SERVICE,
                                  bind_fuchsia_hardware_mailbox::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia_mailbox::CONTROLLER_ID, controller_1_id),
          fdf::MakeAcceptBindRule(bind_fuchsia_mailbox::CHANNEL, 0x1234u),
      },
      (*client_1->parents())[1].bind_rules(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {
          fdf::MakeProperty(bind_fuchsia_hardware_mailbox::SERVICE,
                            bind_fuchsia_hardware_mailbox::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia_mailbox::CONTROLLER_ID, 0u),
          fdf::MakeProperty(bind_fuchsia_mailbox::CHANNEL, 0x1234u),
      },
      (*client_1->parents())[1].properties(), false));
}

}  // namespace mailbox_dt
