// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/devicetree/visitors/drivers/spi-controllers/spi-bus-visitor/spi-bus-visitor.h"

#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/default/mmio/mmio.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <optional>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/spi/cpp/bind.h>
#include <gtest/gtest.h>

namespace spi_bus_dt {

class SpiBusVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<SpiBusVisitor> {
 public:
  explicit SpiBusVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<SpiBusVisitor>(dtb_path, "SpiBusVisitorTest") {}

  std::optional<fuchsia_hardware_platform_bus::Node> FindPbusNode(const char* match) {
    const size_t pbus_node_count =
        env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);
    for (size_t i = 0; i < pbus_node_count; i++) {
      fuchsia_hardware_platform_bus::Node node =
          env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);
      if (node.name() && node.name()->find(match) != std::string::npos) {
        return node;
      }
    }

    return {};
  }

  std::optional<fidl::Request<fuchsia_driver_framework::CompositeNodeManager::AddSpec>>
  FindCompositeNodeSpec(const char* match) {
    const size_t mgr_request_count =
        env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size);
    for (size_t i = 0; i < mgr_request_count; i++) {
      fidl::Request<fuchsia_driver_framework::CompositeNodeManager::AddSpec> spec =
          env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, i);
      if (spec.name() && spec.name()->find(match) != std::string::npos) {
        return spec;
      }
    }

    return {};
  }
};

TEST(SpiBusVisitorTest, TestSpiChannels) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<fdf_devicetree::MmioVisitor>()).is_ok());

  SpiBusVisitorTester* spi_tester = nullptr;
  {
    auto tester = std::make_unique<SpiBusVisitorTester>("/pkg/test-data/spi.dtb");
    spi_tester = tester.get();
    ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());
  }

  ASSERT_EQ(ZX_OK, spi_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(spi_tester->DoPublish().is_ok());

  const std::optional<fuchsia_hardware_platform_bus::Node> pbus_node =
      spi_tester->FindPbusNode("spi-");
  ASSERT_TRUE(pbus_node);

  const std::optional<std::vector<fuchsia_hardware_platform_bus::Metadata>>& metadata =
      pbus_node->metadata();

  ASSERT_TRUE(metadata);
  ASSERT_EQ(2lu, metadata->size());

  // SPI bus metadata
  {
    const std::vector<uint8_t>& metadata_blob = *(*metadata)[0].data();
    fit::result decoded =
        fidl::Unpersist<fuchsia_hardware_spi_businfo::SpiBusMetadata>(cpp20::span(metadata_blob));
    ASSERT_TRUE(decoded.is_ok());
    ASSERT_EQ(decoded->bus_id(), 0u);
    ASSERT_TRUE(decoded->channels());

    const std::vector<fuchsia_hardware_spi_businfo::SpiChannel>& channels = *decoded->channels();
    ASSERT_EQ(channels.size(), 4lu);
    EXPECT_EQ(channels[0].cs(), 0);
    EXPECT_EQ(channels[1].cs(), 1);
    EXPECT_EQ(channels[2].cs(), 2);
    EXPECT_EQ(channels[3].cs(), 3);
  }

  // SPI bus metadata
  {
    const std::vector<uint8_t>& metadata_blob = *(*metadata)[1].data();
    fit::result decoded =
        fidl::Unpersist<fuchsia_hardware_spi_businfo::SpiBusMetadata>(cpp20::span(metadata_blob));
    ASSERT_TRUE(decoded.is_ok());
    ASSERT_EQ(decoded->bus_id(), 0u);
    ASSERT_TRUE(decoded->channels());

    const std::vector<fuchsia_hardware_spi_businfo::SpiChannel>& channels = *decoded->channels();
    ASSERT_EQ(channels.size(), 4lu);
    EXPECT_EQ(channels[0].cs(), 0);
    EXPECT_EQ(channels[1].cs(), 1);
    EXPECT_EQ(channels[2].cs(), 2);
    EXPECT_EQ(channels[3].cs(), 3);
  }

  std::optional<fidl::Request<fuchsia_driver_framework::CompositeNodeManager::AddSpec>>
      child0_spec = spi_tester->FindCompositeNodeSpec("child-0");
  ASSERT_TRUE(child0_spec);
  ASSERT_TRUE(child0_spec->parents());
  ASSERT_EQ(child0_spec->parents()->size(), 2ul);

  // The 0th parent is the board driver.
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {{
          fdf::MakeProperty(bind_fuchsia_hardware_spi::SERVICE,
                            bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia::SPI_CHIP_SELECT, 0u),
      }},
      (*child0_spec->parents())[1].properties(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {{
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spi::SERVICE,
                                  bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia::SPI_BUS_ID, 0u),
          fdf::MakeAcceptBindRule(bind_fuchsia::SPI_CHIP_SELECT, 0u),
      }},
      (*child0_spec->parents())[1].bind_rules(), false));

  std::optional<fidl::Request<fuchsia_driver_framework::CompositeNodeManager::AddSpec>>
      child1_spec = spi_tester->FindCompositeNodeSpec("child-1");
  ASSERT_TRUE(child1_spec);
  ASSERT_TRUE(child1_spec->parents());
  ASSERT_EQ(child1_spec->parents()->size(), 2ul);

  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {{
          fdf::MakeProperty(bind_fuchsia_hardware_spi::SERVICE,
                            bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia::SPI_CHIP_SELECT, 0u),
      }},
      (*child1_spec->parents())[1].properties(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {{
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spi::SERVICE,
                                  bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia::SPI_BUS_ID, 0u),
          fdf::MakeAcceptBindRule(bind_fuchsia::SPI_CHIP_SELECT, 1u),
      }},
      (*child1_spec->parents())[1].bind_rules(), false));

  std::optional<fidl::Request<fuchsia_driver_framework::CompositeNodeManager::AddSpec>>
      child2_spec = spi_tester->FindCompositeNodeSpec("child-2");
  ASSERT_TRUE(child2_spec);
  ASSERT_TRUE(child2_spec->parents());
  ASSERT_EQ(child2_spec->parents()->size(), 3ul);

  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {{
          fdf::MakeProperty(bind_fuchsia_hardware_spi::SERVICE,
                            bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia::SPI_CHIP_SELECT, 0u),
      }},
      (*child2_spec->parents())[1].properties(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {{
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spi::SERVICE,
                                  bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia::SPI_BUS_ID, 0u),
          fdf::MakeAcceptBindRule(bind_fuchsia::SPI_CHIP_SELECT, 2u),
      }},
      (*child2_spec->parents())[1].bind_rules(), false));

  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {{
          fdf::MakeProperty(bind_fuchsia_hardware_spi::SERVICE,
                            bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia::SPI_CHIP_SELECT, 1u),
      }},
      (*child2_spec->parents())[2].properties(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {{
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spi::SERVICE,
                                  bind_fuchsia_hardware_spi::SERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia::SPI_BUS_ID, 0u),
          fdf::MakeAcceptBindRule(bind_fuchsia::SPI_CHIP_SELECT, 3u),
      }},
      (*child2_spec->parents())[2].bind_rules(), false));
}

}  // namespace spi_bus_dt
