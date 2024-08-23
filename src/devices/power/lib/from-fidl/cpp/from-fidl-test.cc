// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/lib/from-fidl/cpp/from-fidl.h"

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace power::from_fidl::test {

// Transition tests.

// Verify that `CreateTransition()` can convert a fuchsia_hardware_power::Transition value into a
// fdf_power::Transition value.
TEST(FromFidlTest, TransitionFromNatural) {
  fuchsia_hardware_power::Transition fidl{{
      .target_level = 2,
      .latency_us = 30045,
  }};
  zx::result transition = CreateTransition(fidl);
  ASSERT_TRUE(transition.is_ok()) << transition.status_string();
  ASSERT_EQ(transition->target_level, 2u);
  ASSERT_EQ(transition->latency_us, 30045u);
}

// Verify `CreateTransition()` will fail if target level is missing.
TEST(ConvertPowerConfigTest, TransitionFromNaturalMissingTargetLevel) {
  fuchsia_hardware_power::Transition fidl{{
      .latency_us = 30045,
  }};
  zx::result transition = CreateTransition(fidl);
  ASSERT_EQ(transition.status_value(), ZX_ERR_INVALID_ARGS);
}

// Verify `CreateTransition()` will fail if latency is missing.
TEST(ConvertPowerConfigTest, TransitionFromNaturalMissingLatency) {
  fuchsia_hardware_power::Transition fidl{{
      .target_level = 2,
  }};
  zx::result transition = CreateTransition(fidl);
  ASSERT_EQ(transition.status_value(), ZX_ERR_INVALID_ARGS);
}

// Level tuple tests.

// Verify that `CreateLevelTuple()` can convert a fuchsia_hardware_power::LevelTuple value into a
// fdf_power::LevelTuple value.
TEST(FromFidlTest, LevelTupleFromNatural) {
  fuchsia_hardware_power::LevelTuple fidl{{
      .child_level = 56,
      .parent_level = 254,
  }};
  zx::result level_tuple = CreateLevelTuple(fidl);
  ASSERT_TRUE(level_tuple.is_ok()) << level_tuple.status_string();
  ASSERT_EQ(level_tuple->child_level, 56u);
  ASSERT_EQ(level_tuple->parent_level, 254u);
}

// Verify `CreateLevelTuple()` will fail if child level is missing.
TEST(FromFidlTest, LevelTupleFromNaturalMissingChildLevel) {
  fuchsia_hardware_power::LevelTuple fidl{{
      .parent_level = 254,
  }};
  zx::result level_tuple = CreateLevelTuple(fidl);
  ASSERT_EQ(level_tuple.status_value(), ZX_ERR_INVALID_ARGS);
}

// Verify `CreateLevelTuple()` will fail if parent level is missing.
TEST(FromFidlTest, LevelTupleFromNaturalMissingParentLevel) {
  fuchsia_hardware_power::LevelTuple fidl{{
      .child_level = 56,
  }};
  zx::result level_tuple = CreateLevelTuple(fidl);
  ASSERT_EQ(level_tuple.status_value(), ZX_ERR_INVALID_ARGS);
}

// Power level tests.

// Verify that `CreatePowerLevel()` can convert a fuchsia_hardware_power::PowerLevel value into a
// fdf_power::PowerLevel value.
TEST(FromFidlTest, PowerLevelFromNatural) {
  fuchsia_hardware_power::PowerLevel fidl{{
      .level = 4,
      .name = "test power level",
      .transitions = {{
          {{
              .target_level = 5,
              .latency_us = 48392,
          }},
          {{
              .target_level = 90,
              .latency_us = 12334,
          }},
      }},
  }};
  zx::result power_level = CreatePowerLevel(fidl);
  ASSERT_TRUE(power_level.is_ok()) << power_level.status_string();
  ASSERT_EQ(power_level->level, 4u);
  ASSERT_EQ(power_level->name, "test power level");
  ASSERT_EQ(power_level->transitions.size(), 2u);

  ASSERT_EQ(power_level->transitions[0].target_level, 5u);
  ASSERT_EQ(power_level->transitions[0].latency_us, 48392u);

  ASSERT_EQ(power_level->transitions[1].target_level, 90u);
  ASSERT_EQ(power_level->transitions[1].latency_us, 12334u);
}

// Verify `CreatePowerLevel()` will fail if level is missing.
TEST(FromFidlTest, PowerLevelFromNaturalMissingLevel) {
  fuchsia_hardware_power::PowerLevel fidl{{
      .name = "test power level",
      .transitions = {{}},
  }};
  zx::result power_level = CreatePowerLevel(fidl);
  ASSERT_EQ(power_level.status_value(), ZX_ERR_INVALID_ARGS);
}

// Verify `CreatePowerLevel()` will fail if name is missing.
TEST(FromFidlTest, PowerLevelFromNaturalMissingName) {
  fuchsia_hardware_power::PowerLevel fidl{{
      .level = 4,
      .transitions = {{}},
  }};
  zx::result power_level = CreatePowerLevel(fidl);
  ASSERT_EQ(power_level.status_value(), ZX_ERR_INVALID_ARGS);
}

// Verify `CreatePowerLevel()` will succeed if transitions are missing.
TEST(FromFidlTest, PowerLevelFromNaturalMissingTransitions) {
  fuchsia_hardware_power::PowerLevel fidl{{
      .level = 4,
      .name = "test power level",
  }};
  zx::result power_level = CreatePowerLevel(fidl);
  ASSERT_TRUE(power_level.is_ok()) << power_level.status_string();
  ASSERT_TRUE(power_level->transitions.empty());
}

// Power element tests.

// Verify that `CreatePowerElement()` can convert a fuchsia_hardware_power::PowerElement value into
// a fdf_power::PowerElement value.
TEST(FromFidlTest, PowerElementFromNatural) {
  fuchsia_hardware_power::PowerElement fidl{
      {.name = "test power element",
       .levels = {{
           {{.level = 200, .name = "test level 1", .transitions = {{}}}},
           {{.level = 134, .name = "test level 2", .transitions = {{}}}},
       }}}};
  zx::result power_element = CreatePowerElement(fidl);
  ASSERT_TRUE(power_element.is_ok()) << power_element.status_string();
  ASSERT_EQ(power_element->name, "test power element");
  ASSERT_EQ(power_element->levels.size(), 2u);

  ASSERT_EQ(power_element->levels[0].level, 200u);
  ASSERT_EQ(power_element->levels[0].name, "test level 1");
  ASSERT_TRUE(power_element->levels[0].transitions.empty());

  ASSERT_EQ(power_element->levels[1].level, 134u);
  ASSERT_EQ(power_element->levels[1].name, "test level 2");
  ASSERT_TRUE(power_element->levels[1].transitions.empty());
}

// Verify `CreatePowerElement()` will fail if name is missing.
TEST(FromFidlTest, PowerElementFromNaturalMissingName) {
  fuchsia_hardware_power::PowerElement fidl{{.levels = {{}}}};
  zx::result power_element = CreatePowerElement(fidl);
  ASSERT_EQ(power_element.status_value(), ZX_ERR_INVALID_ARGS);
}

// Verify `CreatePowerElement()` will succeed if levels are missing.
TEST(FromFidlTest, PowerElementFromNaturalMissingLevels) {
  fuchsia_hardware_power::PowerElement fidl{{
      .name = "test power element",
  }};
  zx::result power_element = CreatePowerElement(fidl);
  ASSERT_TRUE(power_element.is_ok()) << power_element.status_string();
  ASSERT_TRUE(power_element->levels.empty());
}

// Parent element tests.

// Verify that `CreateParentElement()` can convert a fuchsia_hardware_power::ParentElement value
// with type "sag" into a fdf_power::ParentElement value.
TEST(FromFidlTest, ParentElementFromNaturalWithSag) {
  fuchsia_hardware_power::ParentElement fidl = fuchsia_hardware_power::ParentElement::WithSag(
      fuchsia_hardware_power::SagElement::kApplicationActivity);
  zx::result parent_element = CreateParentElement(fidl);
  ASSERT_TRUE(parent_element.is_ok()) << parent_element.status_string();
  ASSERT_EQ(parent_element->type(), fdf_power::ParentElement::Type::kSag);
  ASSERT_EQ(parent_element->GetSag(),
            std::optional<fdf_power::SagElement>{fdf_power::SagElement::kApplicationActivity});
}

// Verify that `CreateParentElement()` can convert a fuchsia_hardware_power::ParentElement value
// with type "instance name" into a fdf_power::ParentElement value.
TEST(FromFidlTest, ParentElementFromNaturalWithInstanceName) {
  fuchsia_hardware_power::ParentElement fidl =
      fuchsia_hardware_power::ParentElement::WithInstanceName("test parent element");
  zx::result parent_element = CreateParentElement(fidl);
  ASSERT_TRUE(parent_element.is_ok()) << parent_element.status_string();
  ASSERT_EQ(parent_element->type(), fdf_power::ParentElement::Type::kInstanceName);
  ASSERT_EQ(parent_element->GetInstanceName(), std::optional<std::string>{"test parent element"});
}

// Power dependency tests.

// Verify that `CreatePowerDependency()` can convert a fuchsia_hardware_power::PowerDependency value
// into a fdf_power::PowerDependency value.
TEST(FromFidlTest, PowerDependencyFromNatural) {
  fuchsia_hardware_power::PowerDependency fidl{
      {.child = "test child",
       .parent = fuchsia_hardware_power::ParentElement::WithInstanceName("test parent element"),
       .level_deps = {{{{.child_level = 111, .parent_level = 0}},
                       {{.child_level = 222, .parent_level = 1}}}},
       .strength = fuchsia_hardware_power::RequirementType::kAssertive}};
  zx::result power_dependency = CreatePowerDependency(fidl);
  ASSERT_TRUE(power_dependency.is_ok()) << power_dependency.status_string();
  ASSERT_EQ(power_dependency->child, "test child");
  ASSERT_EQ(power_dependency->parent,
            fdf_power::ParentElement::WithInstanceName("test parent element"));
  ASSERT_EQ(power_dependency->strength, fdf_power::RequirementType::kAssertive);
  ASSERT_EQ(power_dependency->level_deps.size(), 2u);

  ASSERT_EQ(power_dependency->level_deps[0].child_level, 111u);
  ASSERT_EQ(power_dependency->level_deps[0].parent_level, 0u);

  ASSERT_EQ(power_dependency->level_deps[1].child_level, 222u);
  ASSERT_EQ(power_dependency->level_deps[1].parent_level, 1u);
}

// Verify `CreatePowerDependency()` will fail if child is missing.
TEST(FromFidlTest, PowerDependencyFromNaturalMissingChild) {
  fuchsia_hardware_power::PowerDependency fidl{
      {.parent = fuchsia_hardware_power::ParentElement::WithInstanceName(""),
       .level_deps = {{}},
       .strength = fuchsia_hardware_power::RequirementType::kAssertive}};
  zx::result power_dependency = CreatePowerDependency(fidl);
  ASSERT_EQ(power_dependency.status_value(), ZX_ERR_INVALID_ARGS);
}

// Verify `CreatePowerDependency()` will fail if parent is missing.
TEST(FromFidlTest, PowerDependencyFromNaturalMissingParent) {
  fuchsia_hardware_power::PowerDependency fidl{
      {.child = "",
       .level_deps = {{}},
       .strength = fuchsia_hardware_power::RequirementType::kAssertive}};
  zx::result power_dependency = CreatePowerDependency(fidl);
  ASSERT_EQ(power_dependency.status_value(), ZX_ERR_INVALID_ARGS);
}

// Verify `CreatePowerDependency()` will succeed if level dependencies are
// missing.
TEST(FromFidlTest, PowerDependencyFromNaturalMissingLevelDeps) {
  fuchsia_hardware_power::PowerDependency fidl{
      {.child = "",
       .parent = fuchsia_hardware_power::ParentElement::WithInstanceName(""),
       .strength = fuchsia_hardware_power::RequirementType::kAssertive}};
  zx::result power_dependency = CreatePowerDependency(fidl);
  ASSERT_TRUE(power_dependency.is_ok()) << power_dependency.status_string();
  ASSERT_TRUE(power_dependency->level_deps.empty());
}

// Verify `CreatePowerDependency()` will fail if strength is missing.
TEST(FromFidlTest, PowerDependencyFromNaturalMissingStrength) {
  fuchsia_hardware_power::PowerDependency fidl{
      {.child = "",
       .parent = fuchsia_hardware_power::ParentElement::WithInstanceName(""),
       .level_deps = {{}}}};
  zx::result power_dependency = CreatePowerDependency(fidl);
  ASSERT_EQ(power_dependency.status_value(), ZX_ERR_INVALID_ARGS);
}

// Power element configuration tests.

// Verify that `CreatePowerElementConfiguration()` can convert a
// fuchsia_hardware_power::PowerElementConfiguration value into a
// fdf_power::PowerElementConfiguration value.
TEST(FromFidlTest, PowerElementConfigurationFromNatural) {
  fuchsia_hardware_power::PowerElement element{{.name = "test power element", .levels = {{}}}};

  fuchsia_hardware_power::PowerDependency dependency1{
      {.child = "test child 1",
       .parent = fuchsia_hardware_power::ParentElement::WithInstanceName("test parent element 1"),
       .level_deps = {{}},
       .strength = fuchsia_hardware_power::RequirementType::kAssertive}};

  fuchsia_hardware_power::PowerDependency dependency2{
      {.child = "test child 2",
       .parent = fuchsia_hardware_power::ParentElement::WithInstanceName("test parent element 2"),
       .level_deps = {{}},
       .strength = fuchsia_hardware_power::RequirementType::kOpportunistic}};

  fuchsia_hardware_power::PowerElementConfiguration fidl{
      {.element = std::move(element),
       .dependencies = {{std::move(dependency1), std::move(dependency2)}}}};

  zx::result config = CreatePowerElementConfiguration(fidl);
  ASSERT_TRUE(config.is_ok()) << config.status_string();
  ASSERT_EQ(config->element.name, "test power element");
  ASSERT_TRUE(config->element.levels.empty());
  ASSERT_EQ(config->dependencies.size(), 2u);

  ASSERT_EQ(config->dependencies[0].child, "test child 1");
  ASSERT_EQ(config->dependencies[0].parent,
            fdf_power::ParentElement::WithInstanceName("test parent element 1"));
  ASSERT_TRUE(config->dependencies[0].level_deps.empty());
  ASSERT_EQ(config->dependencies[0].strength, fdf_power::RequirementType::kAssertive);

  ASSERT_EQ(config->dependencies[1].child, "test child 2");
  ASSERT_EQ(config->dependencies[1].parent,
            fdf_power::ParentElement::WithInstanceName("test parent element 2"));
  ASSERT_TRUE(config->dependencies[1].level_deps.empty());
  ASSERT_EQ(config->dependencies[1].strength, fdf_power::RequirementType::kOpportunistic);
}

// Verify `CreatePowerElementConfiguration()` will fail if element is missing.
TEST(FromFidlTest, PowerElementConfigurationFromNaturalMissingElement) {
  fuchsia_hardware_power::PowerElement element{{.name = "test power element", .levels = {{}}}};

  fuchsia_hardware_power::PowerElementConfiguration fidl{{.dependencies = {{}}}};
  zx::result config = CreatePowerElementConfiguration(fidl);
  ASSERT_EQ(config.status_value(), ZX_ERR_INVALID_ARGS);
}

// Verify `CreatePowerElementConfiguration()` will succeed if dependencies are missing.
TEST(FromFidlTest, PowerElementConfigurationFromNaturalMissingDependencies) {
  fuchsia_hardware_power::PowerElement element{{.name = "test power element", .levels = {{}}}};

  fuchsia_hardware_power::PowerElementConfiguration fidl{{.element = std::move(element)}};
  zx::result config = CreatePowerElementConfiguration(fidl);
  ASSERT_TRUE(config.is_ok()) << config.status_string();
  ASSERT_TRUE(config->dependencies.empty());
}

}  // namespace power::from_fidl::test
