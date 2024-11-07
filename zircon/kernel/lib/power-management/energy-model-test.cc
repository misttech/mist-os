// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/power-management/energy-model.h"

#include <lib/stdcompat/array.h>
#include <lib/stdcompat/utility.h>
#include <zircon/errors.h>
#include <zircon/syscalls-next.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <cstddef>

#include <fbl/ref_ptr.h>
#include <gtest/gtest.h>

#include "test-helper.h"

namespace {

using power_management::EnergyModel;
using power_management::PowerDomain;
using power_management::PowerDomainRegistry;

TEST(PowerLevelTest, Ctor) {
  constexpr zx_processor_power_level_t kLevel = {
      .options = 0,
      .processing_rate = 456,
      .power_coefficient_nw = 789,
      .control_interface = cpp23::to_underlying(power_management::ControlInterface::kArmPsci),
      .control_argument = 12345,
      .diagnostic_name = "foobar one two three",
  };

  power_management::PowerLevel level(0, kLevel);

  EXPECT_EQ(level.level(), 0);
  EXPECT_EQ(level.processing_rate(), kLevel.processing_rate);
  EXPECT_EQ(level.power_coefficient_nw(), kLevel.power_coefficient_nw);
  EXPECT_EQ(level.control(),
            static_cast<power_management::ControlInterface>(kLevel.control_interface));
  EXPECT_EQ(level.control_argument(), kLevel.control_argument);
  EXPECT_EQ(level.type(), power_management::PowerLevel::Type::kActive);
  EXPECT_EQ(level.name(), std::string_view(kLevel.diagnostic_name));
  EXPECT_TRUE(level.TargetsPowerDomain());
  EXPECT_FALSE(level.TargetsCpus());
}

TEST(PowerLevelTest, Ctor2) {
  constexpr zx_processor_power_level_t kLevel = {
      .options = ZX_PROCESSOR_POWER_LEVEL_OPTIONS_DOMAIN_INDEPENDENT,
      .processing_rate = 123,
      .power_coefficient_nw = 789,
      .control_interface = cpp23::to_underlying(power_management::ControlInterface::kArmPsci),
      .control_argument = 12345,
      .diagnostic_name = "foobar one two three",
  };

  power_management::PowerLevel level(123, kLevel);

  EXPECT_EQ(level.level(), 123);
  EXPECT_EQ(level.processing_rate(), kLevel.processing_rate);
  EXPECT_EQ(level.power_coefficient_nw(), kLevel.power_coefficient_nw);
  EXPECT_EQ(level.control(),
            static_cast<power_management::ControlInterface>(kLevel.control_interface));
  EXPECT_EQ(level.control_argument(), kLevel.control_argument);
  EXPECT_EQ(level.type(), power_management::PowerLevel::Type::kActive);
  EXPECT_EQ(level.name(), std::string_view(kLevel.diagnostic_name));
  EXPECT_FALSE(level.TargetsPowerDomain());
  EXPECT_TRUE(level.TargetsCpus());
}

TEST(PowerLevelTransitionTest, Ctor) {
  static constexpr zx_processor_power_level_transition_t kTransition = {
      .latency = 456,
      .energy_nj = 1234,
      .from = 0,
      .to = 1,
  };

  power_management::PowerLevelTransition transition(kTransition);

  EXPECT_EQ(transition.latency(), zx_duration_from_nsec(kTransition.latency));
  EXPECT_EQ(transition.energy_cost_nj(), kTransition.energy_nj);
}

TEST(PowerModelTest, Create) {
  static constexpr auto kPowerLevels = cpp20::to_array<zx_processor_power_level_t>({
      {
          .options = 0,
          .processing_rate = 0,
          .power_coefficient_nw = 1,
          .control_interface = cpp23::to_underlying(power_management::ControlInterface::kArmPsci),
          .control_argument = 1,
          .diagnostic_name = "0",
      },
      {
          .options = 0,
          .processing_rate = 4,
          .power_coefficient_nw = 8,
          .control_interface = cpp23::to_underlying(power_management::ControlInterface::kCpuDriver),
          .control_argument = 3,
          .diagnostic_name = "1",
      },
      {
          .options = 0,
          .processing_rate = 0,
          .power_coefficient_nw = 2,
          .control_interface = cpp23::to_underlying(power_management::ControlInterface::kArmWfi),
          .control_argument = 0,
          .diagnostic_name = "2",
      },
      {
          .options = 0,
          .processing_rate = 4,
          .power_coefficient_nw = 10,
          .control_interface = cpp23::to_underlying(power_management::ControlInterface::kCpuDriver),
          .control_argument = 1,
          .diagnostic_name = "3",
      },
  });

  static constexpr auto kTransitions = cpp20::to_array<zx_processor_power_level_transition_t>({
      {
          .latency = 1,
          .energy_nj = 2,
          .from = 1,
          .to = 0,
      },
      {

          .latency = 2,
          .energy_nj = 3,
          .from = 2,
          .to = 0,

      },
      {

          .latency = 3,
          .energy_nj = 4,
          .from = 3,
          .to = 0,
      },
      {

          .latency = 1,
          .energy_nj = 2,
          .from = 0,
          .to = 1,
      },
      {

          .latency = 3,
          .energy_nj = 4,
          .from = 2,
          .to = 1,
      },
      {

          .latency = 4,
          .energy_nj = 5,
          .from = 3,
          .to = 1,
      },
      {

          .latency = 2,
          .energy_nj = 3,
          .from = 0,
          .to = 2,
      },
      {

          .latency = 3,
          .energy_nj = 4,
          .from = 1,
          .to = 2,
      },
      {
          .latency = 5,
          .energy_nj = 6,
          .from = 3,
          .to = 2,
      },
      {

          .latency = 3,
          .energy_nj = 4,
          .from = 0,
          .to = 3,

      },
      {

          .latency = 4,
          .energy_nj = 5,
          .from = 1,
          .to = 3,
      },
      {

          .latency = 5,
          .energy_nj = 6,
          .from = 2,
          .to = 3,
      },
  });

  auto energy_model = EnergyModel::Create(kPowerLevels, kTransitions);
  ASSERT_TRUE(energy_model.is_ok());

  // Proper transformation of the model and the transition table.
  auto check_level = [](const power_management::PowerLevel& actual,
                        const power_management::PowerLevel& expected) {
    EXPECT_EQ(actual.level(), expected.level());
    EXPECT_EQ(actual.control(), expected.control());
    EXPECT_EQ(actual.control_argument(), expected.control_argument());
    EXPECT_EQ(actual.name(), expected.name());
    EXPECT_EQ(actual.power_coefficient_nw(), expected.power_coefficient_nw());
    EXPECT_EQ(actual.processing_rate(), expected.processing_rate());
    EXPECT_EQ(actual.type(), expected.type());
    EXPECT_EQ(actual.TargetsCpus(), expected.TargetsCpus());
    EXPECT_EQ(actual.TargetsPowerDomain(), expected.TargetsPowerDomain());
  };

  ASSERT_EQ(energy_model->levels().size(), 4u);
  auto levels = energy_model->levels();
  for (size_t i = 0; i < levels.size() - 1; ++i) {
    size_t j = i + 1;
    EXPECT_LE(levels[i].processing_rate(), levels[i].processing_rate());
    if (levels[i].processing_rate() == levels[j].processing_rate()) {
      EXPECT_LE(levels[i].power_coefficient_nw(), levels[j].power_coefficient_nw());
    }
    check_level(levels[i],
                power_management::PowerLevel(levels[i].level(), kPowerLevels[levels[i].level()]));
    check_level(levels[j],
                power_management::PowerLevel(levels[j].level(), kPowerLevels[levels[j].level()]));
  }

  auto get_original_transition = [&levels](size_t i, size_t j) {
    size_t og_i = levels[i].level();
    size_t og_j = levels[j].level();
    for (const auto& transition : kTransitions) {
      if (transition.from == og_i && transition.to == og_j) {
        return power_management::PowerLevelTransition(transition);
      }
    }
    return power_management::PowerLevelTransition::Invalid();
  };

  auto transitions = energy_model->transitions();
  for (size_t i = 0; i < levels.size(); ++i) {
    for (size_t j = 0; j < levels.size(); ++j) {
      auto transition = transitions[i][j];
      auto og_transition = get_original_transition(i, j);
      EXPECT_EQ(transition.latency(), og_transition.latency());
      EXPECT_EQ(transition.energy_cost_nj(), og_transition.energy_cost_nj());
    }
  }

  // Properly partitioned.
  ASSERT_EQ(energy_model->idle_levels().size(), 2u);
  EXPECT_EQ(energy_model->idle_levels()[0].level(), 0u);
  EXPECT_EQ(energy_model->idle_levels()[1].level(), 2u);

  ASSERT_EQ(energy_model->active_levels().size(), 2u);
  EXPECT_EQ(energy_model->active_levels()[0].level(), 1u);
  EXPECT_EQ(energy_model->active_levels()[1].level(), 3u);

  // Sorter by tuple <Control Interface, Control Argument>
  for (size_t i = 0; i < levels.size(); ++i) {
    auto& level = levels[i];
    EXPECT_EQ(energy_model->FindPowerLevel(level.control(), level.control_argument()), i);
  }
  EXPECT_FALSE(energy_model->FindPowerLevel(power_management::ControlInterface::kArmPsci, 495));
  EXPECT_FALSE(
      energy_model->FindPowerLevel(static_cast<power_management::ControlInterface>(495), 0));
}

TEST(PowerModelTest, CreateWithEmptyTransitionsIsOk) {
  static constexpr auto kPowerLevels = cpp20::to_array<zx_processor_power_level_t>({
      {
          .options = 0,
          .processing_rate = 0,
          .power_coefficient_nw = 1,
          .control_interface = cpp23::to_underlying(power_management::ControlInterface::kArmPsci),
          .control_argument = 1,
          .diagnostic_name = "0",
      },
      {
          .options = 0,
          .processing_rate = 4,
          .power_coefficient_nw = 8,
          .control_interface = cpp23::to_underlying(power_management::ControlInterface::kCpuDriver),
          .control_argument = 3,
          .diagnostic_name = "1",
      },
      {
          .options = 0,
          .processing_rate = 0,
          .power_coefficient_nw = 2,
          .control_interface = cpp23::to_underlying(power_management::ControlInterface::kArmWfi),
          .control_argument = 0,
          .diagnostic_name = "2",
      },
      {
          .options = 0,
          .processing_rate = 4,
          .power_coefficient_nw = 10,
          .control_interface = cpp23::to_underlying(power_management::ControlInterface::kCpuDriver),
          .control_argument = 1,
          .diagnostic_name = "3",
      },
  });

  auto energy_model = EnergyModel::Create(kPowerLevels, {});
  ASSERT_TRUE(energy_model.is_ok());

  // Proper transformation of the model and the transition table.
  auto check_level = [](const power_management::PowerLevel& actual,
                        const power_management::PowerLevel& expected) {
    EXPECT_EQ(actual.level(), expected.level());
    EXPECT_EQ(actual.control(), expected.control());
    EXPECT_EQ(actual.control_argument(), expected.control_argument());
    EXPECT_EQ(actual.name(), expected.name());
    EXPECT_EQ(actual.power_coefficient_nw(), expected.power_coefficient_nw());
    EXPECT_EQ(actual.processing_rate(), expected.processing_rate());
    EXPECT_EQ(actual.type(), expected.type());
    EXPECT_EQ(actual.TargetsCpus(), expected.TargetsCpus());
    EXPECT_EQ(actual.TargetsPowerDomain(), expected.TargetsPowerDomain());
  };

  ASSERT_EQ(energy_model->levels().size(), 4u);
  auto levels = energy_model->levels();
  for (size_t i = 0; i < levels.size() - 1; ++i) {
    size_t j = i + 1;
    EXPECT_LE(levels[i].processing_rate(), levels[i].processing_rate());
    if (levels[i].processing_rate() == levels[j].processing_rate()) {
      EXPECT_LE(levels[i].power_coefficient_nw(), levels[j].power_coefficient_nw());
    }
    check_level(levels[i],
                power_management::PowerLevel(levels[i].level(), kPowerLevels[levels[i].level()]));
    check_level(levels[j],
                power_management::PowerLevel(levels[j].level(), kPowerLevels[levels[j].level()]));
  }

  for (size_t row = 0; row < energy_model->levels().size(); ++row) {
    for (size_t column = 0; column < energy_model->levels().size(); ++column) {
      const power_management::PowerLevelTransition& transition =
          energy_model->transitions()[row][column];
      EXPECT_EQ(transition.energy_cost_nj(),
                power_management::PowerLevelTransition::Zero().energy_cost_nj());
      EXPECT_EQ(transition.latency(), power_management::PowerLevelTransition::Zero().latency());
    }
  }

  ASSERT_EQ(energy_model->idle_levels().size(), 2u);
  EXPECT_EQ(energy_model->idle_levels()[0].level(), 0u);
  EXPECT_EQ(energy_model->idle_levels()[1].level(), 2u);

  ASSERT_EQ(energy_model->active_levels().size(), 2u);
  EXPECT_EQ(energy_model->active_levels()[0].level(), 1u);
  EXPECT_EQ(energy_model->active_levels()[1].level(), 3u);

  // Sorter by tuple <Control Interface, Control Argument>
  for (size_t i = 0; i < levels.size(); ++i) {
    auto& level = levels[i];
    EXPECT_EQ(energy_model->FindPowerLevel(level.control(), level.control_argument()), i);
  }
  EXPECT_FALSE(energy_model->FindPowerLevel(power_management::ControlInterface::kArmPsci, 495));
  EXPECT_FALSE(
      energy_model->FindPowerLevel(static_cast<power_management::ControlInterface>(495), 0));
}

TEST(PowerDomainRegistryTest, RegisterUniquePowerDomains) {
  PowerDomainRegistry registry;
  std::map<size_t, fbl::RefPtr<PowerDomain>> registed_domains;
  registed_domains[0] = MakePowerDomainHelper(0, 1, 2, 3);
  registed_domains[1] = MakePowerDomainHelper(1, 4, 5, 6);
  registed_domains[2] = MakePowerDomainHelper(2, 7, 8, 9);

  std::map<size_t, fbl::RefPtr<PowerDomain>> cpu_domains;
  auto domain_update = [&cpu_domains](size_t num, auto domain) {
    cpu_domains[num] = std::move(domain);
  };

  for (auto [domain_id, domain] : registed_domains) {
    EXPECT_EQ(domain->total_normalized_utilization(), 0u);
    ASSERT_TRUE(registry.Register(domain, domain_update).is_ok());
  }

  // Validate the domains currently registered.
  size_t domains = 0;
  int cpus = 0;
  registry.Visit([&cpus, &domains, &registed_domains, &cpu_domains](const PowerDomain& domain) {
    domains++;
    auto& expected = registed_domains[domain.id()];
    EXPECT_EQ(expected.get(), &domain);
    ForEachCpuIn(domain.cpus(), [&domain, &cpu_domains, &cpus](size_t num_cpu) {
      EXPECT_EQ(&domain, cpu_domains[num_cpu].get());
      cpus++;
    });
  });
  EXPECT_EQ(domains, registed_domains.size());
  EXPECT_EQ(cpu_domains.size(), 9u);
  EXPECT_EQ(cpus, 9);
}

TEST(PowerDomainRegistryTest, FindDomain) {
  PowerDomainRegistry registry;
  std::map<size_t, fbl::RefPtr<PowerDomain>> registed_domains;
  registed_domains[0] = MakePowerDomainHelper(0, 1, 2, 3);
  registed_domains[1] = MakePowerDomainHelper(1, 4, 5, 6);
  registed_domains[2] = MakePowerDomainHelper(2, 7, 8, 9);

  auto domain_update = [](size_t num, auto domain) {};

  for (auto& [domain_id, domain] : registed_domains) {
    EXPECT_EQ(domain->total_normalized_utilization(), 0u);
    ASSERT_TRUE(registry.Register(domain, domain_update).is_ok());
  }

  EXPECT_EQ(registry.Find(0).get(), registed_domains[0].get());
  EXPECT_EQ(registry.Find(1).get(), registed_domains[1].get());
  EXPECT_EQ(registry.Find(2).get(), registed_domains[2].get());
  EXPECT_EQ(registry.Find(112345567).get(), nullptr);
}

TEST(PowerDomainRegistryTest, UnregisterDomain) {
  PowerDomainRegistry registry;
  std::map<size_t, fbl::RefPtr<PowerDomain>> registed_domains;
  registed_domains[0] = MakePowerDomainHelper(0, 1, 2, 3);
  registed_domains[1] = MakePowerDomainHelper(1, 4, 5, 6);
  registed_domains[2] = MakePowerDomainHelper(2, 7, 8, 9);

  std::map<size_t, fbl::RefPtr<PowerDomain>> cpu_domains;
  auto domain_update = [&cpu_domains](size_t num, auto domain) {
    cpu_domains[num] = std::move(domain);
  };

  for (auto& [domain_id, domain] : registed_domains) {
    EXPECT_EQ(domain->total_normalized_utilization(), 0u);
    ASSERT_TRUE(registry.Register(domain, domain_update).is_ok());
  }

  auto check_registry = [&](size_t current_domain, size_t current_cpus) {
    size_t domains = 0;
    size_t cpus = 0;
    registry.Visit([&cpus, &domains, &registed_domains, &cpu_domains](const PowerDomain& domain) {
      domains++;
      auto& expected = registed_domains[domain.id()];
      EXPECT_EQ(expected.get(), &domain);
      ForEachCpuIn(domain.cpus(), [&domain, &cpu_domains, &cpus](size_t num_cpu) {
        EXPECT_EQ(&domain, cpu_domains[num_cpu].get());
        cpus++;
      });
    });
    EXPECT_EQ(domains, current_domain);
    EXPECT_EQ(cpu_domains.size(), current_cpus);
    EXPECT_EQ(cpus, current_cpus);
  };

  auto clear_domain = [&cpu_domains](size_t num) { cpu_domains.erase(num); };

  EXPECT_EQ(registry.Unregister(12345, clear_domain).status_value(), ZX_ERR_NOT_FOUND);
  EXPECT_TRUE(registry.Unregister(0, clear_domain).is_ok());
  check_registry(2, 6);
  EXPECT_TRUE(registry.Unregister(1, clear_domain).is_ok());
  check_registry(1, 3);
  EXPECT_TRUE(registry.Unregister(2, clear_domain).is_ok());
  check_registry(0, 0);
}

// The purpose of this test is to demonstrate the process for updating immutable characteristics of
// an already registered domain.
TEST(PowerDomainRegistryTest, UpdatingOverlapping) {
  PowerDomainRegistry registry;
  std::map<size_t, fbl::RefPtr<PowerDomain>> registed_domains;
  registed_domains[0] = MakePowerDomainHelper(0, 1, 2, 3);
  registed_domains[1] = MakePowerDomainHelper(1, 4, 5, 6);
  registed_domains[2] = MakePowerDomainHelper(2, 7, 8, 9);
  registed_domains[3] = MakePowerDomainHelper(3, 1, 4, 7, 10);

  std::map<size_t, fbl::RefPtr<PowerDomain>> cpu_domains;
  auto domain_update = [&cpu_domains](size_t num, auto domain) {
    if (!domain) {
      cpu_domains.erase(num);
    } else {
      cpu_domains[num] = std::move(domain);
    }
  };

  // Succeeds, but as seen below domain 3 will fail due to overlapping with existing domain.
  for (auto [domain_id, domain] : registed_domains) {
    if (domain_id != 3) {
      ASSERT_TRUE(registry.Register(domain, domain_update).is_ok());
    } else {
      ASSERT_FALSE(registry.Register(domain, domain_update).is_ok());
    }
  }

  // First we will introduce an updated version of 0-2 that exclude the overlapping cpus.
  registed_domains[0] = MakePowerDomainHelper(0, 2, 3);
  registed_domains[1] = MakePowerDomainHelper(1, 5, 6);
  registed_domains[2] = MakePowerDomainHelper(2, 8, 9);
  for (size_t domain_id = 0; domain_id <= 3; ++domain_id) {
    auto& domain = registed_domains[domain_id];
    ASSERT_TRUE(registry.Register(domain, domain_update).is_ok());
  }

  // Validate the domains currently registered.
  size_t domains = 0;
  int cpus = 0;
  registry.Visit([&cpus, &domains, &registed_domains, &cpu_domains](const PowerDomain& domain) {
    domains++;
    auto& expected = registed_domains[domain.id()];
    EXPECT_EQ(expected.get(), &domain);
    ForEachCpuIn(domain.cpus(), [&domain, &cpu_domains, &cpus](size_t num_cpu) {
      EXPECT_EQ(&domain, cpu_domains[num_cpu].get());
      cpus++;
    });
  });
  EXPECT_EQ(domains, registed_domains.size());
  EXPECT_EQ(cpu_domains.size(), 10u);
  EXPECT_EQ(cpus, 10);
}

TEST(PowerDomainRegistryTest, OverlappingIsError) {
  PowerDomainRegistry registry;
  std::map<size_t, fbl::RefPtr<PowerDomain>> registed_domains;
  registed_domains[0] = MakePowerDomainHelper(0, 1, 2, 3);
  registed_domains[1] = MakePowerDomainHelper(1, 4, 5, 6);
  registed_domains[2] = MakePowerDomainHelper(2, 7, 8, 9);
  registed_domains[3] = MakePowerDomainHelper(3, 1, 4, 7, 10);

  std::map<size_t, fbl::RefPtr<PowerDomain>> cpu_domains;
  auto domain_update = [&cpu_domains](size_t num, auto domain) {
    cpu_domains[num] = std::move(domain);
  };

  for (auto [domain_id, domain] : registed_domains) {
    auto res = registry.Register(domain, domain_update);
    ASSERT_TRUE((domain_id != 3 && res.is_ok()) || (domain_id == 3 && !res.is_ok()));
  }

  // Validate the domains currently registered.
  size_t domains = 0;
  int cpus = 0;
  registry.Visit([&cpus, &domains, &registed_domains, &cpu_domains](const PowerDomain& domain) {
    domains++;
    auto& expected = registed_domains[domain.id()];
    EXPECT_EQ(expected.get(), &domain);
    ForEachCpuIn(domain.cpus(), [&domain, &cpu_domains, &cpus](size_t num_cpu) {
      EXPECT_EQ(&domain, cpu_domains[num_cpu].get());
      cpus++;
    });
  });
  EXPECT_EQ(domains, registed_domains.size() - 1);
  EXPECT_EQ(cpu_domains.size(), 9u);
  EXPECT_EQ(cpus, 9);
}

TEST(PowerDomainRegistryTest, RegisterWithSameIdIsUpdate) {
  PowerDomainRegistry registry;
  std::map<size_t, fbl::RefPtr<PowerDomain>> registed_domains;
  registed_domains[0] = MakePowerDomainHelper(0, 1, 2, 3);
  registed_domains[1] = MakePowerDomainHelper(1, 4, 5, 6);
  registed_domains[2] = MakePowerDomainHelper(2, 7, 8, 9);

  std::map<size_t, fbl::RefPtr<PowerDomain>> cpu_domains;
  auto domain_update = [&cpu_domains](size_t num, auto domain) {
    if (!domain) {
      cpu_domains.erase(num);
    } else {
      cpu_domains[num] = std::move(domain);
    }
  };

  for (auto [domain_id, domain] : registed_domains) {
    ASSERT_TRUE(registry.Register(domain, domain_update).is_ok());
  }

  registed_domains[1] = MakePowerDomainHelper(1, 4, 10);
  ASSERT_TRUE(registry.Register(registed_domains[1], domain_update).is_ok());

  // Validate the domains currently registered.
  size_t domains = 0;
  int cpus = 0;
  registry.Visit([&cpus, &domains, &registed_domains, &cpu_domains](const PowerDomain& domain) {
    domains++;
    auto& expected = registed_domains[domain.id()];
    EXPECT_EQ(expected.get(), &domain);
    ForEachCpuIn(domain.cpus(), [&domain, &cpu_domains, &cpus](size_t num_cpu) {
      EXPECT_EQ(&domain, cpu_domains[num_cpu].get());
      cpus++;
    });
  });

  EXPECT_EQ(domains, registed_domains.size());
  EXPECT_EQ(cpu_domains.size(), 8u);
  EXPECT_EQ(cpus, 8);

  EXPECT_EQ(cpu_domains.count(5), 0u);
  EXPECT_EQ(cpu_domains.count(6), 0u);
}

}  // namespace
