// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_ENERGY_MODEL_H_
#define ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_ENERGY_MODEL_H_

#include <lib/fit/function.h>
#include <lib/stdcompat/array.h>
#include <lib/stdcompat/span.h>
#include <lib/stdcompat/utility.h>
#include <lib/zx/result.h>
#include <stdint.h>
#include <stdlib.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/syscalls-next.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <atomic>
#include <cstdint>
#include <limits>
#include <string_view>

#include <fbl/intrusive_single_list.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>

#include "power-level-controller.h"

namespace power_management {

// forward declaration.
class EnergyModel;

// Enum representing supported control interfaces.
enum class ControlInterface : uint64_t {
  kCpuDriver = ZX_PROCESSOR_POWER_CONTROL_CPU_DRIVER,
  kArmPsci = ZX_PROCESSOR_POWER_CONTROL_ARM_PSCI,
  kArmWfi = ZX_PROCESSOR_POWER_CONTROL_ARM_WFI,
  kRiscvSbi = ZX_PROCESSOR_POWER_CONTROL_RISCV_SBI,
  kRiscvWfi = ZX_PROCESSOR_POWER_CONTROL_RISCV_WFI,
};

constexpr const char* ToString(ControlInterface control_interface) {
  switch (control_interface) {
    case ControlInterface::kCpuDriver:
      return "CPU_DRIVER";
    case ControlInterface::kArmPsci:
      return "ARM_PSCI";
    case ControlInterface::kArmWfi:
      return "ARM_WFI";
    case ControlInterface::kRiscvSbi:
      return "RISCV_SBI";
    case ControlInterface::kRiscvWfi:
      return "RISCV_WFI";
    default:
      return "[unknown]";
  }
}

// List of support control interfaces.
static constexpr auto kSupportedControlInterfaces = cpp20::to_array(
    {ControlInterface::kArmPsci, ControlInterface::kArmWfi, ControlInterface::kRiscvSbi,
     ControlInterface::kRiscvWfi, ControlInterface::kCpuDriver});

// Returns whether the interface is a supported or not.
constexpr bool IsSupportedControlInterface(zx_processor_power_control_t interface) {
  for (auto supported_interface : kSupportedControlInterfaces) {
    if (supported_interface == static_cast<ControlInterface>(interface)) {
      return true;
    }
  }
  return false;
}

// Returns whether the interface is handled by the kernel or not.
constexpr bool IsKernelControlInterface(ControlInterface interface) {
  return interface != ControlInterface::kCpuDriver;
}

// Kernel representation of `zx_processor_power_level_t` with useful accessors and option support.
class PowerLevel {
 public:
  enum Type {
    // Entity is not eligible for active work.
    kIdle,

    // Entity is eligible for work, but the rate at which work is completed is determined by the
    // active power level.
    kActive,
  };

  constexpr PowerLevel() = default;
  explicit PowerLevel(uint8_t level_index, const zx_processor_power_level_t& level)
      : options_(level.options),
        control_(static_cast<ControlInterface>(level.control_interface)),
        control_argument_(level.control_argument),
        processing_rate_(level.processing_rate),
        power_coefficient_nw_(level.power_coefficient_nw),
        level_(level_index) {
    memcpy(name_.data(), level.diagnostic_name, name_.size());
    size_t end = std::string_view(name_.data(), name_.size()).find('\0');
    name_len_ = end == std::string_view::npos ? ZX_MAX_NAME_LEN : end;
  }

  // Power level type. Idle and Active power levels are orthogonal, that is, an entity may be idle
  // while keepings its active power level unchanged. This means that the actual power level of
  // an entity should be determined by the tuple <Idle Power Level*, Active Power Level>, where if
  // `Idle Power Level`is absent then that means that the entity is active and the active power
  // level should be used.
  //
  // This situation happens for example, when a CPU transitions from an active power level A
  // (which may be interpreted as a known OPP or P-State) into an idle state, such as suspension,
  // idle thread or even powering it off.
  constexpr Type type() const { return processing_rate_ == 0 ? Type::kIdle : kActive; }

  // Processing rate when this power level is active. This is key to determining the available
  // bandwidth of the entity.
  constexpr uint64_t processing_rate() const { return processing_rate_; }

  // Relative to the system power consumption, determines how much power is being consumed at this
  // level. This allows determining if this power level should be a candidate when operating under
  // a given energy budget.
  constexpr uint64_t power_coefficient_nw() const { return power_coefficient_nw_; }

  // ID of the interface handling transitions for TO this power level.
  constexpr ControlInterface control() const { return control_; }

  // Argument to be interpreted by the control interface in order to transition to this level.
  //
  // The control interface is only aware of this arguments, and power levels are identified by
  // this argument.
  constexpr uint64_t control_argument() const { return control_argument_; }

  // This level may be transitioned in a per cpu basis, without affecting other entities in the
  // same power domain.
  constexpr bool TargetsCpus() const {
    return (options_ & ZX_PROCESSOR_POWER_LEVEL_OPTIONS_DOMAIN_INDEPENDENT) != 0;
  }

  // This level may be transitioned in a per power domain basis, that is, all other entities in
  // the power domain will be transitioned together.
  //
  // This means that underlying hardware elements are share and it is not possible to transition a
  // single member of the power domain.
  constexpr bool TargetsPowerDomain() const {
    return (options_ & ZX_PROCESSOR_POWER_LEVEL_OPTIONS_DOMAIN_INDEPENDENT) == 0;
  }

  // Name used to identify this power level, for diagnostic purposes.
  constexpr std::string_view name() const { return {name_.data(), name_len_}; }

  // Power Level as understood from the original model perspective.
  constexpr uint8_t level() const { return level_; }

 private:
  // Options.
  zx_processor_power_level_options_t options_ = {};

  // Control interface used to transition to this level.
  ControlInterface control_ = {};

  // Argument to be provided to the control interface.
  uint64_t control_argument_ = 0;

  // Processing rate.
  uint64_t processing_rate_ = 0;

  // Power coefficient in nanowatts.
  uint64_t power_coefficient_nw_ = 0;

  std::array<char, ZX_MAX_NAME_LEN> name_ = {};
  size_t name_len_ = 0;

  // Level as described in the model shared with user.
  uint8_t level_ = 0;
  [[maybe_unused]] std::array<uint8_t, 7> reserved_ = {};
};

// Represents an entry in a transition matrix, where the position in the matrix denotes
// the source and target power level. This constructs just denotes the properties of that
// cell.
class PowerLevelTransition {
 public:
  // Returns an invalid transition.
  static constexpr PowerLevelTransition Invalid() { return {}; }
  static constexpr PowerLevelTransition Zero() {
    return PowerLevelTransition(
        zx_processor_power_level_transition_t{.latency = 0, .energy_nj = 0});
  }

  constexpr PowerLevelTransition() = default;
  explicit constexpr PowerLevelTransition(const zx_processor_power_level_transition_t& transition)
      : latency_(zx_duration_from_nsec(transition.latency)),
        energy_cost_nj_(transition.energy_nj) {}

  // Latency for transitioning from a given level to another.
  constexpr zx_duration_t latency() const { return latency_; }

  // Energy cost in nano joules(nj) for transition from a given level to another.
  constexpr uint64_t energy_cost_nj() const { return energy_cost_nj_; }

  // Whether the transition is valid or not.
  explicit constexpr operator bool() {
    return latency_ != Invalid().latency() && energy_cost_nj_ != Invalid().energy_cost_nj_;
  }

 private:
  // Time required for the transition to take effect. In some cases it may mean for the actual
  // voltage to stabilize.
  zx_duration_t latency_ = ZX_TIME_INFINITE;

  // Amount of energy consumed to perform the transition.
  uint64_t energy_cost_nj_ = std::numeric_limits<uint64_t>::max();
};

// Represents a view of the `zx_processor_power_level_transition_t` array as
// a matrix. As per a view's concept, this view is only valid so long the original
// object remains valid and it's tied to its lifecycle.
//
// Additionally transition matrix are required to be squared matrixes, since
// they describe transition from every existent level to every other level.
struct TransitionMatrix {
 public:
  constexpr TransitionMatrix(const TransitionMatrix& other) = default;

  constexpr cpp20::span<const PowerLevelTransition> operator[](size_t index) const {
    return transitions_.subspan(index * num_rows_, num_rows_);
  }

 private:
  friend EnergyModel;
  TransitionMatrix(cpp20::span<const PowerLevelTransition> transitions, size_t num_rows)
      : transitions_(transitions), num_rows_(num_rows) {
    ZX_DEBUG_ASSERT(transitions_.size() != 0);
    ZX_DEBUG_ASSERT(num_rows_ != 0);
    ZX_DEBUG_ASSERT(transitions_.size() % num_rows_ == 0);
    ZX_DEBUG_ASSERT(transitions.size() / num_rows_ == num_rows_);
  }

  const cpp20::span<const PowerLevelTransition> transitions_;
  const size_t num_rows_;
};

// EnergyModel describes the power consumption rates of the available active and
// idle states a processor may enter, which interfaces to use to effect state
// transitions, and properties of the state transitions, such as energy cost and
// latency. This information is used to make efficient scheduling and load
// balancing decisions that meet the power vs. performance tradeoffs specified
// by a product.
//
// An energy model is constant once initialized. Updating the active energy
// model requires replacing it with a new instance and incrementing a generation
// count to detect stale values derived from the previous energy model.
class EnergyModel {
 public:
  static zx::result<EnergyModel> Create(
      cpp20::span<const zx_processor_power_level_t> levels,
      cpp20::span<const zx_processor_power_level_transition_t> transitions);

  EnergyModel() = default;
  EnergyModel(const EnergyModel&) = delete;
  EnergyModel(EnergyModel&&) = default;

  // All power levels described in the model, sorted by processing power and energy consumption.
  //
  // (1) The processing rate of power level i is less or equal than the processing rate of power
  // level j, where i <= j.
  //
  // (2) The energy cost of power level i is less or equal than the processing rate of power level
  // j, where i <= j.
  constexpr cpp20::span<const PowerLevel> levels() const { return power_levels_; }

  // Following the same rules as `levels()` but returns only the set of power levels whose type is
  // `PowerLevel::Type::kIdle`. This set may be empty.
  constexpr cpp20::span<const PowerLevel> idle_levels() const {
    return levels().subspan(0, idle_power_levels_);
  }

  // Returns the idle power level with the maximum power consumption.
  constexpr std::optional<uint8_t> max_idle_power_level() const {
    if (idle_power_levels_ > 0) {
      return idle_power_levels_ - 1;
    }
    return std::nullopt;
  }

  // Returns the power coefficient of the idle power level with the maximum power consumption. This
  // idle power level typically corresponds to clock gating, such that the power consumption is
  // almost entirely leakage power loss.
  constexpr std::optional<uint64_t> max_idle_power_coefficient_nw() const {
    if (idle_power_levels_ > 0) {
      return power_levels_[idle_power_levels_ - 1].power_coefficient_nw();
    }
    return std::nullopt;
  }

  // Returns the control interface of the idle power level with the maximum power consumption.
  constexpr std::optional<ControlInterface> max_idle_power_level_interface() const {
    if (idle_power_levels_ > 0) {
      return power_levels_[idle_power_levels_ - 1].control();
    }
    return std::nullopt;
  }

  // Following the same rules as `levels()` but returns only the set of power levels whose type is
  // `PowerLevel::Type::kActive`. This set may be empty.
  constexpr cpp20::span<const PowerLevel> active_levels() const {
    return levels().subspan(idle_power_levels_);
  }

  // Returns a transition matrix, where the entry <i,j> represents the transition costs for
  // transitioning from i to j.
  TransitionMatrix transitions() const {
    return TransitionMatrix(transitions_, power_levels_.size());
  }

  std::optional<uint8_t> FindPowerLevel(ControlInterface interface_id,
                                        uint64_t control_argument) const;

 private:
  EnergyModel(fbl::Vector<PowerLevel> levels, fbl::Vector<PowerLevelTransition> transitions,
              fbl::Vector<size_t> control_lookup, size_t idle_levels)
      : power_levels_(std::move(levels)),
        transitions_(std::move(transitions)),
        control_lookup_(std::move(control_lookup)),
        idle_power_levels_(idle_levels) {}

  fbl::Vector<PowerLevel> power_levels_;
  fbl::Vector<PowerLevelTransition> transitions_;
  fbl::Vector<size_t> control_lookup_;
  size_t idle_power_levels_ = 0;
};

// PowerDomain establishes the relationship between a set of CPUs, the energy
// model that describes their characteristics, and the power level controller
// responsible for changing the active power levels for the set of CPUs.
//
// Instances of PowerDomain are safe for concurrent use.
class PowerDomain : public fbl::RefCounted<PowerDomain>,
                    public fbl::SinglyLinkedListable<fbl::RefPtr<PowerDomain>> {
 public:
  PowerDomain(uint32_t id, zx_cpu_set_t cpus, EnergyModel model)
      : PowerDomain(id, cpus, std::move(model), nullptr) {}
  PowerDomain(uint32_t id, zx_cpu_set_t cpus, EnergyModel model,
              fbl::RefPtr<PowerLevelController> controller)
      : cpus_(cpus), id_(id), energy_model_(std::move(model)), controller_(std::move(controller)) {}

  // ID representing the relationship between a set of CPUs and a power model.
  constexpr uint32_t id() const { return id_; }

  // Set of CPUs associated with `model()`.
  constexpr const zx_cpu_set_t& cpus() const { return cpus_; }

  // Model describing the behavior of the power domain.
  constexpr const EnergyModel& model() const { return energy_model_; }

  // The total normalized utilization of the set of processors associated with this power domain.
  //
  // Uses relaxed semantics, since the value does not need to synchronize with other memory accesses
  // and innaccuracy is acceptable.
  uint64_t total_normalized_utilization() const {
    return total_normalized_utilization_.load(std::memory_order_relaxed);
  }
  // Handler for transitions where the target level's control interface is not kernel handled.
  const fbl::RefPtr<PowerLevelController>& controller() const { return controller_; }

  // Returns whether the kernel scheduler should send power level update requests to the controller.
  // This is used by tests to prevent the scheduler from sending power level change requests through
  // the fake control interface that could confuse the test. It does not prevent the kernel from
  // exercising the control interface, however, only whether the scheduler will interact with the
  // control interface to handle utilization changes.
  //
  // Uses relaxed semantics, since this variable will generally be synchronized by the lock
  // protecting each PowerState as it is associated with this PowerDomain. However, an atomic is
  // used to prevent formal data races if the value is read outside of external synchronization,
  // which can't be statically checked internally.
  bool scheduler_control_enabled() const {
    return scheduler_control_enabled_.load(std::memory_order_relaxed);
  }

  // Sets whether the kernel scheduler should send power level update requests through the control
  // interface. When set to false, the scheduler must not send requests to avoid confusing tests.
  void SetSchedulerControlEnabled(bool enabled) {
    scheduler_control_enabled_.store(enabled, std::memory_order_relaxed);
  }

 private:
  friend class PowerState;

  const zx_cpu_set_t cpus_;
  const uint32_t id_;
  const EnergyModel energy_model_;

  std::atomic<uint64_t> total_normalized_utilization_{0};
  const fbl::RefPtr<PowerLevelController> controller_ = nullptr;

  std::atomic<bool> scheduler_control_enabled_ = false;
};

// `PowerDomainRegistry` provides a starting point for looking at any
// of the previously registered power domains.
//
// This class also provides the mechanism for updating existing `PowerDomain` entries, by means of
// replacing. For atomic updates external synchronization is required.
//
// In practice, there will be a single instance of this object in the kernel.
class PowerDomainRegistry {
 public:
  // Register `power_domain` with this registry.
  //
  // A `CpuPowerDomainAccessor` must provide the following contract:
  //
  //  // `cpu_num` is a valid cpu number that falls within `cpu_set_t` bits.
  //  // `new_domain` new `PowerDomain` for `cpu_num`. If `nullptr` then
  //  //  current domain should be cleared.
  //  //  void operator()(size_t cpu_num, fbl::RefPtr<PowerDomain>& new_domain)
  //
  template <typename CpuPowerDomainAccessor>
  zx::result<> Register(fbl::RefPtr<PowerDomain> power_domain,
                        CpuPowerDomainAccessor&& update_domain) {
    return UpdateRegistry(std::move(power_domain), update_domain);
  }

  // Register `power_domain` with this registry.
  //
  // A `CpuPowerDomainAccessor` must provide the following contract:
  //
  //  // `cpu_num` is a valid cpu number that falls within `cpu_set_t` bits.
  //  //  current domain should be cleared.
  //  //  void operator()(size_t cpu_num)
  template <typename CpuPowerDomainAccessor>
  zx::result<> Unregister(uint32_t domain_id, CpuPowerDomainAccessor&& clear_domain) {
    return RemoveFromRegistry(domain_id, clear_domain);
  }

  // Returns a reference to a `PowerDomain` whose id matches `domain_id`.
  // Returns `nullptr` if there is no match.
  fbl::RefPtr<PowerDomain> Find(uint32_t domain_id) const {
    for (auto& domain : domains_) {
      if (domain.id() == domain_id) {
        return fbl::RefPtr(const_cast<PowerDomain*>(&domain));
      }
    }
    return nullptr;
  }

  // Visit each registered `PowerDomain`.
  template <typename Visitor>
  void Visit(Visitor&& visitor) {
    for (const auto& domain : domains_) {
      visitor(domain);
    }
  }

 private:
  static constexpr size_t kBitsPerBucket = ZX_CPU_SET_BITS_PER_WORD;
  static constexpr size_t kBuckets = ZX_CPU_SET_MAX_CPUS / ZX_CPU_SET_BITS_PER_WORD;

  // Updates the registry list, by possibly removing a domain registered with the same id.
  // If a domain is replaced.
  zx::result<> UpdateRegistry(
      fbl::RefPtr<PowerDomain> power_domain,
      fit::inline_function<void(size_t, fbl::RefPtr<PowerDomain>)> update_cpu_power_domain);

  // Dissociates `domain_id` from all the cpus and removes it from the registry.
  zx::result<> RemoveFromRegistry(uint32_t domain_id,
                                  fit::inline_function<void(size_t)> clear_domain);

  fbl::SinglyLinkedList<fbl::RefPtr<PowerDomain>> domains_;
};

}  // namespace power_management

#endif  // ZIRCON_KERNEL_LIB_POWER_MANAGEMENT_INCLUDE_LIB_POWER_MANAGEMENT_ENERGY_MODEL_H_
