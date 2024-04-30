// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>

#include <src/lib/testing/loop_fixture/test_loop_fixture.h>

#include "lib/fidl/cpp/wire/channel.h"
#include "sdk/lib/driver/power/cpp/element-description-builder.h"
#include "sdk/lib/driver/power/cpp/element-description.h"
#include "sdk/lib/driver/power/cpp/power-support.h"
#include "zircon/syscalls/object.h"

namespace power_lib_test {
class PowerLibTest : public gtest::TestLoopFixture {};

void check_channels_peered(zx_handle_t c1, zx_handle_t c2) {
  zx_info_handle_basic_t basic1;
  size_t actual1;
  size_t handles1;

  zx_info_handle_basic_t basic2;
  size_t actual2;
  size_t handles2;

  zx_object_get_info(c1, ZX_INFO_HANDLE_BASIC, &basic1, sizeof(zx_info_handle_basic_t), &actual1,
                     &handles1);
  zx_object_get_info(c2, ZX_INFO_HANDLE_BASIC, &basic2, sizeof(zx_info_handle_basic_t), &actual2,
                     &handles2);
  ASSERT_EQ(basic1.koid, basic2.related_koid);
}

TEST_F(PowerLibTest, ElementBuilderFilledOut) {
  fuchsia_hardware_power::wire::PowerElementConfiguration config;
  fdf_power::TokenMap tokens;

  zx_handle_t active, passive;
  zx_event_create(0, &active);
  zx_event_create(0, &passive);
  zx::event active_event(active);
  zx::event passive_event(passive);
  fidl::Endpoints<fuchsia_power_broker::CurrentLevel> current_level =
      fidl::CreateEndpoints<fuchsia_power_broker::CurrentLevel>().value();
  fidl::Endpoints<fuchsia_power_broker::RequiredLevel> required_level =
      fidl::CreateEndpoints<fuchsia_power_broker::RequiredLevel>().value();
  fidl::Endpoints<fuchsia_power_broker::Lessor> lessor =
      fidl::CreateEndpoints<fuchsia_power_broker::Lessor>().value();

  fdf_power::ElementDesc desc = fdf_power::ElementDescBuilder(config, std::move(tokens))
                                    .SetActiveToken(active_event.borrow())
                                    .SetPassiveToken(passive_event.borrow())
                                    .SetCurrentLevel(std::move(current_level.server))
                                    .SetRequiredLevel(std::move(required_level.server))
                                    .SetLessor(std::move(lessor.server))
                                    .Build();

  ASSERT_TRUE(desc.lessor_server_.is_valid());
  ASSERT_TRUE(desc.level_control_servers_.first.is_valid());
  ASSERT_TRUE(desc.level_control_servers_.second.is_valid());

  ASSERT_TRUE(desc.active_token_.is_valid());
  ASSERT_TRUE(desc.passive_token_.is_valid());

  ASSERT_EQ(desc.current_level_client_, std::nullopt);
  ASSERT_EQ(desc.required_level_client_, std::nullopt);
  ASSERT_EQ(desc.lessor_client_, std::nullopt);

  check_channels_peered(current_level.client.handle()->get(),
                        desc.level_control_servers_.first.handle()->get());
  check_channels_peered(required_level.client.handle()->get(),
                        desc.level_control_servers_.second.handle()->get());
  check_channels_peered(lessor.client.handle()->get(), desc.lessor_server_.handle()->get());
}

TEST_F(PowerLibTest, ElementBuilderMissingCurrentLevel) {
  fuchsia_hardware_power::wire::PowerElementConfiguration config;
  fdf_power::TokenMap tokens;

  zx_handle_t active, passive;
  zx_event_create(0, &active);
  zx_event_create(0, &passive);
  zx::event active_event(active);
  zx::event passive_event(passive);
  fidl::Endpoints<fuchsia_power_broker::RequiredLevel> required_level =
      fidl::CreateEndpoints<fuchsia_power_broker::RequiredLevel>().value();
  fidl::Endpoints<fuchsia_power_broker::Lessor> lessor =
      fidl::CreateEndpoints<fuchsia_power_broker::Lessor>().value();

  fdf_power::ElementDesc desc = fdf_power::ElementDescBuilder(config, std::move(tokens))
                                    .SetActiveToken(active_event.borrow())
                                    .SetPassiveToken(passive_event.borrow())
                                    .SetRequiredLevel(std::move(required_level.server))
                                    .SetLessor(std::move(lessor.server))
                                    .Build();

  ASSERT_TRUE(desc.lessor_server_.is_valid());
  ASSERT_TRUE(desc.level_control_servers_.first.is_valid());
  ASSERT_TRUE(desc.level_control_servers_.second.is_valid());

  ASSERT_TRUE(desc.active_token_.is_valid());
  ASSERT_TRUE(desc.passive_token_.is_valid());

  ASSERT_TRUE(desc.current_level_client_.has_value());
  ASSERT_EQ(desc.required_level_client_, std::nullopt);
  ASSERT_EQ(desc.lessor_client_, std::nullopt);

  check_channels_peered(desc.current_level_client_->handle()->get(),
                        desc.level_control_servers_.first.handle()->get());
  check_channels_peered(required_level.client.handle()->get(),
                        desc.level_control_servers_.second.handle()->get());
  check_channels_peered(lessor.client.handle()->get(), desc.lessor_server_.handle()->get());
}

TEST_F(PowerLibTest, ElementBuilderMin) {
  fuchsia_hardware_power::wire::PowerElementConfiguration config;
  fdf_power::TokenMap tokens;
  fdf_power::ElementDesc desc = fdf_power::ElementDescBuilder(config, std::move(tokens)).Build();

  ASSERT_NE(desc.current_level_client_, std::nullopt);
  ASSERT_TRUE(desc.current_level_client_.value().is_valid());

  ASSERT_NE(desc.required_level_client_, std::nullopt);
  ASSERT_TRUE(desc.required_level_client_.value().is_valid());

  ASSERT_NE(desc.lessor_client_, std::nullopt);
  ASSERT_TRUE(desc.required_level_client_.value().is_valid());

  ASSERT_TRUE(desc.lessor_server_.is_valid());
  ASSERT_TRUE(desc.level_control_servers_.first.is_valid());
  ASSERT_TRUE(desc.level_control_servers_.second.is_valid());

  ASSERT_TRUE(desc.active_token_.is_valid());
  ASSERT_TRUE(desc.passive_token_.is_valid());

  check_channels_peered(desc.current_level_client_->handle()->get(),
                        desc.level_control_servers_.first.handle()->get());
  check_channels_peered(desc.required_level_client_->handle()->get(),
                        desc.level_control_servers_.second.handle()->get());
  check_channels_peered(desc.lessor_client_->handle()->get(), desc.lessor_server_.handle()->get());
}

}  // namespace power_lib_test
