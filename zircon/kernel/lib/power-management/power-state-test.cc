
// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/power-management/power-state.h"

#include <lib/power-management/energy-model.h>
#include <zircon/syscalls-next.h>

#include <cstdint>

#include <gtest/gtest.h>

#include "test-helper.h"

namespace {

using power_management::ControlInterface;
using power_management::PowerDomain;
using power_management::PowerLevelUpdateRequest;
using power_management::PowerState;

constexpr uint32_t kModelId = 123;
constexpr uint32_t kTotalPowerLevels = 8;

TEST(PowerStateTest, Default) {
  PowerState state;
  EXPECT_EQ(state.domain(), nullptr);
  EXPECT_FALSE(state.is_serving());
  EXPECT_FALSE(state.active_power_level());
  EXPECT_FALSE(state.desired_active_power_level());
  EXPECT_EQ(0u, state.active_power_coefficient_nw());
  EXPECT_FALSE(state.max_idle_power_level());
  EXPECT_EQ(0u, state.max_idle_power_coefficient_nw());
  EXPECT_FALSE(state.max_idle_power_level_interface());
  EXPECT_EQ(0u, state.normalized_utilization());
}

TEST(PowerStateTest, SetOrUpdateDomainSetsModel) {
  PowerState state;
  fbl::RefPtr<PowerDomain> domain = MakePowerDomainHelper(kModelId, 0, 1, 2, 3, 4, 5);

  EXPECT_FALSE(state.SetOrUpdateDomain(domain));

  EXPECT_EQ(state.domain(), domain);
  EXPECT_FALSE(state.active_power_level());
  EXPECT_FALSE(state.desired_active_power_level());
  EXPECT_EQ(0u, state.active_power_coefficient_nw());
  EXPECT_EQ(kMaxIdlePowerLevel, state.max_idle_power_level());
  EXPECT_EQ(kMaxIdlePowerLevel + 1u, state.max_idle_power_coefficient_nw());
  EXPECT_EQ(ControlInterface::kArmWfi, state.max_idle_power_level_interface());
}

TEST(PowerStateTest, SetOrUpdateDomainKeepsPowerLevelWhenSameModelId) {
  PowerState state;
  fbl::RefPtr<PowerDomain> domain = MakePowerDomainHelper(kModelId, 0, 1, 2, 3, 4, 5);
  fbl::RefPtr<PowerDomain> domain2 = MakePowerDomainHelper(kModelId, 0, 1, 2, 3, 4);

  EXPECT_FALSE(state.SetOrUpdateDomain(domain));
  ASSERT_EQ(state.domain(), domain);
  ASSERT_EQ(ZX_OK, state.UpdateActivePowerLevel(kMinActivePowerLevel).status_value());

  ASSERT_EQ(state.SetOrUpdateDomain(domain2), domain);
  EXPECT_EQ(state.domain(), domain2);
  EXPECT_EQ(state.active_power_level(), kMinActivePowerLevel);
  EXPECT_FALSE(state.desired_active_power_level());
}

TEST(PowerStateTest, SetOrUpdateDomainClearsPowerLevelWhenDifferentModelId) {
  PowerState state;
  fbl::RefPtr<PowerDomain> domain = MakePowerDomainHelper(kModelId, 0, 1, 2, 3, 4, 5);
  fbl::RefPtr<PowerDomain> domain2 = MakePowerDomainHelper(kModelId + 1, 0, 1, 2, 3, 4);

  EXPECT_FALSE(state.SetOrUpdateDomain(domain));
  ASSERT_EQ(state.domain(), domain);
  ASSERT_EQ(ZX_OK, state.UpdateActivePowerLevel(kMinActivePowerLevel).status_value());

  ASSERT_EQ(state.SetOrUpdateDomain(domain2), domain);
  EXPECT_EQ(state.domain(), domain2);
  EXPECT_FALSE(state.active_power_level());
  EXPECT_FALSE(state.desired_active_power_level());
}

TEST(PowerStateTest, SetOrUpdateDomainNullptrClearsState) {
  PowerState state;
  fbl::RefPtr<PowerDomain> domain = MakePowerDomainHelper(kModelId, 0, 1, 2, 3, 4, 5);

  EXPECT_FALSE(state.SetOrUpdateDomain(domain));
  ASSERT_EQ(state.domain(), domain);
  ASSERT_EQ(ZX_OK, state.UpdateActivePowerLevel(kMinActivePowerLevel).status_value());

  ASSERT_EQ(state.SetOrUpdateDomain(nullptr), domain);
  EXPECT_EQ(state.domain(), nullptr);
  EXPECT_FALSE(state.active_power_level());
  EXPECT_FALSE(state.desired_active_power_level());
}

TEST(PowerStateTest, TransitionWhenModelIsUnknown) {
  PowerState state;
  EXPECT_FALSE(state.RequestTransition(1, 8));
}

TEST(PowerStateTest, TransitionWhenPowerLevelIsUnknown) {
  PowerState state;
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 0, 1, 2, 3, 4, 5);

  EXPECT_FALSE(state.SetOrUpdateDomain(domain));
  ASSERT_EQ(state.domain(), domain);

  EXPECT_FALSE(state.RequestTransition(1, kMinActivePowerLevel + 1));
}

TEST(PowerStateTest, TransitionWhenPowerLevelIsDesiredPowerLevel) {
  PowerState state;
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 0, 1, 2, 3, 4, 5);

  EXPECT_FALSE(state.SetOrUpdateDomain(domain));
  ASSERT_EQ(state.domain(), domain);
  ASSERT_EQ(ZX_OK, state.UpdateActivePowerLevel(kMinActivePowerLevel).status_value());

  EXPECT_FALSE(state.RequestTransition(1, kMinActivePowerLevel));
}

TEST(PowerStateTest, TransitionWhenPowerLevelIsTooHigh) {
  PowerState state;
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 0, 1, 2, 3, 4, 5);

  EXPECT_FALSE(state.SetOrUpdateDomain(domain));
  ASSERT_EQ(state.domain(), domain);
  ASSERT_EQ(ZX_OK, state.UpdateActivePowerLevel(kMinActivePowerLevel).status_value());

  EXPECT_DEATH(state.RequestTransition(1, kTotalPowerLevels), "ASSERT FAILED");
}

TEST(PowerStateTest, TransitionWhenPowerLevelIsTooLow) {
  PowerState state;
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 0, 1, 2, 3, 4, 5);

  EXPECT_FALSE(state.SetOrUpdateDomain(domain));
  ASSERT_EQ(state.domain(), domain);
  ASSERT_EQ(ZX_OK, state.UpdateActivePowerLevel(kMinActivePowerLevel).status_value());

  EXPECT_DEATH(state.RequestTransition(1, kMaxIdlePowerLevel), "ASSERT FAILED");
}

TEST(PowerStateTest, UpdateUtilizationReflectsOnDomain) {
  PowerState state;
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 0, 1, 2, 3, 4, 5);
  auto domain2 = MakePowerDomainHelper(kModelId + 1, energy_model, 0, 1, 2, 3, 4, 5);

  EXPECT_EQ(0u, state.normalized_utilization());
  EXPECT_EQ(0u, domain->total_normalized_utilization());
  EXPECT_EQ(0u, domain2->total_normalized_utilization());

  // Update utilization before associating with a domain.
  state.UpdateUtilization(24);

  EXPECT_EQ(24u, state.normalized_utilization());
  EXPECT_EQ(0u, domain->total_normalized_utilization());
  EXPECT_EQ(0u, domain2->total_normalized_utilization());

  // Associating with a domain should update the domain total.
  EXPECT_FALSE(state.SetOrUpdateDomain(domain));
  ASSERT_EQ(state.domain(), domain);

  EXPECT_EQ(24u, domain->total_normalized_utilization());
  EXPECT_EQ(0u, domain2->total_normalized_utilization());

  // Update utilization while associated with a domain.
  state.UpdateUtilization(-10);

  EXPECT_EQ(14u, state.normalized_utilization());
  EXPECT_EQ(14u, domain->total_normalized_utilization());
  EXPECT_EQ(0u, domain2->total_normalized_utilization());

  // Changing to a different domain should move the utilization from the previous domain to the new
  // domain.
  EXPECT_EQ(domain, state.SetOrUpdateDomain(domain2));

  EXPECT_EQ(14u, state.normalized_utilization());
  EXPECT_EQ(0u, domain->total_normalized_utilization());
  EXPECT_EQ(14u, domain2->total_normalized_utilization());
}

TEST(PowerLevelTransitionTest, PortPacket) {
  PowerLevelUpdateRequest request = {
      .domain_id = 1,
      .target_id = 2,
      .control = ControlInterface::kArmWfi,
      .control_argument = 12345,
      .options = 1221212121,
  };

  zx_port_packet_t port_packet = request.port_packet();

  EXPECT_EQ(port_packet.key, request.domain_id);
  EXPECT_EQ(port_packet.type, ZX_PKT_TYPE_PROCESSOR_POWER_LEVEL_TRANSITION_REQUEST);
  EXPECT_EQ(port_packet.status, ZX_OK);
  EXPECT_EQ(port_packet.processor_power_level_transition.domain_id, request.target_id);
  EXPECT_EQ(port_packet.processor_power_level_transition.control_interface,
            static_cast<uint64_t>(request.control));
  EXPECT_EQ(port_packet.processor_power_level_transition.control_argument,
            request.control_argument);
}

}  // namespace
