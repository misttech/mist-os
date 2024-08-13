// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_POWER_CPP_TYPES_H_
#define LIB_DRIVER_POWER_CPP_TYPES_H_

#include <lib/zx/handle.h>
#include <lib/zx/result.h>

#include <optional>
#include <variant>
#include <vector>

namespace fdf_power {

// The length of time it takes to move to a power level.
struct Transition {
  // The power level we're moving to.
  uint8_t target_level;

  // The time it takes to move to the level in microseconds.
  uint32_t latency_us;
};

// A zero-indexed set of levels that a device can assume.
struct PowerLevel {
  // The zero-indexed level of this `PowerLevel`.
  uint8_t level;

  // Human-readable label for this `PowerLevel`, used only for debugging.
  std::string name;

  // Describes the levels that are valid transitions from this `PowerLevel`.
  std::vector<Transition> transitions;
};

// Set of `PowerLevel`s and a human-readable identifier. A `PowerLevel` itself contains information
// about valid transitions out of that level.
struct PowerElement {
  std::string name;
  std::vector<PowerLevel> levels;
};

// Represents a dependency between two power levels of two different `PowerElement`s.
struct LevelTuple {
  uint8_t child_level;
  uint8_t parent_level;
};

enum class RequirementType : uint32_t {
  kAssertive = 1,
  kOpportunistic = 2,
};

// NOTE: This is _not_ a complete list of SAG elements. This enum only lists the elements supported
// by the power support library.
enum class SagElement : uint32_t {
  kExecutionState = 1,
  kExecutionResumeLatency = 2,
  kWakeHandling = 3,
  kApplicationActivity = 4,
};

// Identifier for an element that is another element's parent, in other words an element that the
// other element depends upon.
class ParentElement {
 public:
  enum class Type {
    // The parent element's access token should be available from a `PowerTokenProvider` capability
    // in the incoming namespace and should provide this same string in the `GetToken` response.
    kName,

    // The parent element is one of SAG's elements and the access token should be obtained from the
    // appropriate SAG-related protocol.
    kSag,

    // The parent element's access token should be available from
    // /svc/fuchsia.hardware.power.PowerTokenProvider/{instance_name}`.
    kInstanceName,
  };

  static ParentElement WithName(std::string name);

  static ParentElement WithSag(SagElement sag);

  static ParentElement WithInstanceName(std::string instance_name);

  // Sets the type to `kName`.
  void SetName(std::string name);

  // Sets the type to `kSag`.
  void SetSag(SagElement sag);

  // Sets the type to `kInstanceName`.
  void SetInstanceName(std::string instance_name);

  // Returns nullopt if type is not `kName`.
  std::optional<std::string> GetName() const;

  // Returns nullopt if type is not `kSag`.
  std::optional<SagElement> GetSag() const;

  // Returns nullopt if type is not `kInstanceName`.
  std::optional<std::string> GetInstanceName() const;

  Type type() const { return type_; }

  bool operator==(const ParentElement& rhs) const;

 private:
  using ValueType = std::variant<std::string, SagElement, std::string>;

  // The index of the value associated with `type` stored within `value_`. This index isn't stored
  // in `Type` because it is considered an implementation detail.
  static constexpr size_t kNameIndex = 0;
  static constexpr size_t kSagIndex = 1;
  static constexpr size_t kInstanceNameIndex = 2;

  explicit ParentElement(Type type, ValueType value) : type_(type), value_(std::move(value)) {}

  Type type_;
  ValueType value_;
};

// Describes the relationship between the `PowerLevel`s of two `PowerElement`s. `child` is the name
// of the `PowerElement` which has `PowerLevel`s that depend on `parent`.
struct PowerDependency {
  // The name for a `PowerElement` which a driver owns.
  std::string child;

  // The name for a `PowerElement` which a driver has access to
  ParentElement parent;

  // The map of level dependencies from `child` to `parent`.
  std::vector<LevelTuple> level_deps;

  RequirementType strength;
};

// Contains the `PowerElement` description and any dependencies it has on other `PowerElement`s.
struct PowerElementConfiguration {
  PowerElement element;
  std::vector<PowerDependency> dependencies;
};

}  // namespace fdf_power

#endif  // LIB_DRIVER_POWER_CPP_TYPES_H_
