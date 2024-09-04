
// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/power-management/power-state.h"

#include <lib/power-management/energy-model.h>
#include <zircon/syscalls-next.h>

#include <cstdint>
#include <optional>

#include <gtest/gtest.h>

#include "test-helper.h"

namespace {

using power_management::PowerModel;
using power_management::PowerState;

constexpr uint32_t kModelId = 123;
constexpr uint32_t kTotalPowerLevels = 8;
constexpr uint8_t kDomainDependantPowerLevel = kTotalPowerLevels - 1;

TEST(PowerStateTest, Ctor) {
  {
    PowerState state;

    ASSERT_EQ(state.domain(), nullptr);
    ASSERT_FALSE(state.active_power_level());
    ASSERT_FALSE(state.desired_active_power_level());
    ASSERT_FALSE(state.idle_power_level());
    ASSERT_FALSE(state.power_level());
    ASSERT_FALSE(state.IsActive());
    ASSERT_FALSE(state.IsIdle());
  }

  {
    auto domain = MakePowerDomainHelper(kModelId, 1, 2, 3, 4, 5, 6);
    PowerState state(domain, std::nullopt, 2, 4);

    ASSERT_EQ(state.domain(), domain);
    ASSERT_EQ(state.active_power_level(), 2);
    ASSERT_EQ(state.power_level(), 2);
    ASSERT_EQ(state.desired_active_power_level(), 4);
    ASSERT_FALSE(state.idle_power_level());
    ASSERT_TRUE(state.IsActive());
    ASSERT_FALSE(state.IsIdle());
  }

  {
    auto domain = MakePowerDomainHelper(kModelId, 1, 2, 3, 4, 5, 6);
    PowerState state(domain, 5, 2, 4);

    ASSERT_EQ(state.domain(), domain);
    ASSERT_EQ(state.active_power_level(), 2);
    ASSERT_EQ(state.idle_power_level(), 5);
    ASSERT_EQ(state.power_level(), 5);
    ASSERT_EQ(state.desired_active_power_level(), 4);
    ASSERT_FALSE(state.IsActive());
    ASSERT_TRUE(state.IsIdle());
  }

  // Many states impacting a single omain.
  {
    auto domain = MakePowerDomainHelper(kModelId, 1, 2, 3, 4, 5, 6);
    PowerState state(domain, 5, 2, 4);
    PowerState state2(domain, std::nullopt, 3, 4);
  }
}

TEST(PowerStateTest, SetOrUpdateDomainSetsModel) {
  PowerState state;
  auto domain = MakePowerDomainHelper(kModelId, 1, 2, 3, 4, 5, 6);

  // No previous domain.
  ASSERT_EQ(state.SetOrUpdateDomain(domain), nullptr);

  ASSERT_EQ(state.domain(), domain);
  ASSERT_FALSE(state.active_power_level());
  ASSERT_FALSE(state.desired_active_power_level());
}

TEST(PowerStateTest, SetOrUpdateDomainKeepsPowerLevelWhenSameModelId) {
  auto domain = MakePowerDomainHelper(kModelId, 1, 2, 3, 4, 5, 6);
  auto domain_2 = MakePowerDomainHelper(kModelId, 1, 2, 3, 4, 5);
  PowerState state(domain, std::nullopt, 2, 2);

  // No previous domain.
  ASSERT_EQ(state.SetOrUpdateDomain(domain_2), domain);

  ASSERT_EQ(state.domain(), domain_2);
  ASSERT_EQ(state.active_power_level(), 2);
  ASSERT_EQ(state.desired_active_power_level(), 2);
}

TEST(PowerStateTest, SetOrUpdateDomainClearsPowerLevelWhenDifferentModelId) {
  auto domain = MakePowerDomainHelper(kModelId, 1, 2, 3, 4, 5, 6);
  auto domain_2 = MakePowerDomainHelper(kModelId + 1, 1, 2, 3, 4, 5);
  PowerState state(domain, std::nullopt, 2, 2);

  // No previous domain.
  ASSERT_EQ(state.SetOrUpdateDomain(domain_2), domain);

  ASSERT_EQ(state.domain(), domain_2);
  ASSERT_FALSE(state.active_power_level());
  ASSERT_FALSE(state.desired_active_power_level());
}

TEST(PowerStateTest, TransitionWhenModelIsUnknown) {
  PowerState state;
  EXPECT_FALSE(state.RequestTransition(1, 8));
}

TEST(PowerStateTest, TransitionWhenPowerLevelIsUnknown) {
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 1, 2, 3, 4, 5, 6);
  PowerState state(domain, std::nullopt, std::nullopt, std::nullopt);
  EXPECT_FALSE(state.RequestTransition(1, 0));
}

TEST(PowerStateTest, TransitionWhenPowerLevelIsDesiredPowerLevel) {
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 1, 2, 3, 4, 5, 6);
  PowerState state(domain, std::nullopt, 2, 2);
  EXPECT_FALSE(state.RequestTransition(1, 2));
}

TEST(PowerStateTest, TransitionWhenPowerLevelIsTooHigh) {
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 1, 2, 3, 4, 5, 6);
  PowerState state(domain, std::nullopt, 2, 2);
  EXPECT_FALSE(state.RequestTransition(1, kTotalPowerLevels));
}

TEST(PowerStateTest, TransitionWhenPowerLevelWhenDesiredStateChanges) {
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 1, 2, 3, 4, 5, 6);
  PowerState state(domain, std::nullopt, 2, 2);
  auto request_or = state.RequestTransition(1, kDomainDependantPowerLevel);
  static_assert(kDomainIndependantPowerLevel != kDomainDependantPowerLevel);
  ASSERT_TRUE(request_or);

  EXPECT_EQ(request_or->target_id, kModelId);
  EXPECT_EQ(request_or->options, 0u);
  EXPECT_EQ(request_or->control_argument, ControlInterfaceArgForLevel(kDomainDependantPowerLevel));
  EXPECT_EQ(request_or->control, ControlInterfaceIdForLevel(kDomainDependantPowerLevel));
}

TEST(PowerStateTest, TransitionWhenPowerLevelWhenDesiredStateChangesDomainIndependantLevel) {
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 1, 2, 3, 4, 5, 6);
  PowerState state(domain, std::nullopt, 2, kTotalPowerLevels - 1);
  auto request_or = state.RequestTransition(1, kDomainIndependantPowerLevel);
  ASSERT_TRUE(request_or);

  EXPECT_EQ(request_or->target_id, 1u);
  EXPECT_EQ(request_or->options, ZX_PROCESSOR_POWER_LEVEL_OPTIONS_DOMAIN_INDEPENDENT);
  EXPECT_EQ(request_or->control_argument,
            ControlInterfaceArgForLevel(kDomainIndependantPowerLevel));
  EXPECT_EQ(request_or->control, ControlInterfaceIdForLevel(kDomainIndependantPowerLevel));
}

TEST(PowerStateTest, UpdatePowerLevelToIdle) {
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 1, 2, 3, 4, 5, 6);
  PowerState state(domain, std::nullopt, 2, 2);

  ASSERT_TRUE(state.IsActive());
  size_t i = 0;
  auto& level = domain->model().levels()[i];
  ASSERT_TRUE(level.type() == power_management::PowerLevel::kIdle);
  auto res = state.UpdatePowerLevel(level.control(), level.control_argument());
  ASSERT_TRUE(res.is_ok());
  EXPECT_EQ(state.power_level(), i);
  EXPECT_EQ(state.idle_power_level(), i);
  EXPECT_EQ(state.desired_active_power_level(), 2);
  EXPECT_TRUE(state.IsIdle());
  EXPECT_FALSE(state.IsActive());
}

TEST(PowerStateTest, UpdatePowerLevelToActive) {
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 1, 2, 3, 4, 5, 6);
  PowerState state(domain, 0, 2, kTotalPowerLevels - 1);

  ASSERT_TRUE(state.IsIdle());

  size_t i = kTotalPowerLevels - 1;
  auto& level = domain->model().levels()[i];
  ASSERT_TRUE(level.type() == power_management::PowerLevel::kActive);
  // Emulate updating an OPP while the CPU is idle.
  auto res = state.UpdatePowerLevel(level.control(), level.control_argument());

  ASSERT_TRUE(res.is_ok());
  EXPECT_EQ(state.power_level(), 0);
  EXPECT_TRUE(state.idle_power_level());
  EXPECT_EQ(state.active_power_level(), i);
  EXPECT_EQ(state.desired_active_power_level(), kTotalPowerLevels - 1);
  EXPECT_TRUE(state.IsIdle());
  EXPECT_FALSE(state.IsActive());

  // The CPU wakes up and transitions from idle.
  state.TransitionFromIdle();

  // Now the power state reflects the active state.
  EXPECT_EQ(state.power_level(), i);
  EXPECT_FALSE(state.idle_power_level());
  EXPECT_EQ(state.active_power_level(), i);
  EXPECT_EQ(state.desired_active_power_level(), i);
  EXPECT_FALSE(state.IsIdle());
  EXPECT_TRUE(state.IsActive());
}

TEST(PowerStateTest, UpdateUtilizationReflectsOnDomain) {
  auto energy_model = MakeFakeEnergyModel(kTotalPowerLevels);
  auto domain = MakePowerDomainHelper(kModelId, energy_model, 1, 2, 3, 4, 5, 6);
  PowerState state(domain, 0, 2, kTotalPowerLevels - 1);

  state.UpdateUtilization(24);

  EXPECT_EQ(domain->total_normalized_utilization(), 24u);
}

}  // namespace
