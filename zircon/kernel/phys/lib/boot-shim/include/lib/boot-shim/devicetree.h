// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_H_
#define ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_H_

#include <lib/boot-shim/devicetree-boot-shim.h>
#include <lib/boot-shim/item-base.h>
#include <lib/boot-shim/watchdog.h>
#include <lib/devicetree/devicetree.h>
#include <lib/devicetree/matcher.h>
#include <lib/fit/function.h>
#include <lib/fit/result.h>
#include <lib/memalloc/range.h>
#include <lib/stdcompat/array.h>
#include <lib/stdcompat/source_location.h>
#include <lib/stdcompat/string_view.h>
#include <lib/uart/all.h>
#include <lib/zbi-format/cpu.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/memory.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/item.h>
#include <lib/zbitl/storage-traits.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <algorithm>
#include <cstdint>
#include <optional>
#include <string_view>
#include <type_traits>
#include <variant>

#include <fbl/intrusive_hash_table.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/type_info.h>

namespace boot_shim {

// Base class for DevicetreeItems, providing default implementations for the Matcher API.
// Derived classes MUST implement OnNode.
template <typename T, size_t MaxScans>
class DevicetreeItemBase {
 public:
  static constexpr size_t kMaxScans = MaxScans;

  constexpr DevicetreeItemBase() = default;
  constexpr DevicetreeItemBase(const char* shim_name, FILE* log)
      : log_(log), shim_name_(shim_name) {}

  devicetree::ScanState OnNode(const devicetree::NodePath&, const devicetree::PropertyDecoder&) {
    static_assert(kMaxScans != MaxScans, "Must implement OnNode.");
    return devicetree::ScanState::kActive;
  }

  void OnError(std::string_view error) {
    Log("Error on %s, %*s\n", fbl::TypeInfo<T>::Name(), static_cast<int>(error.length()),
        error.data());
  }

  devicetree::ScanState OnSubtree(const devicetree::NodePath&) {
    return devicetree::ScanState::kActive;
  }

  devicetree::ScanState OnScan() { return devicetree::ScanState::kActive; }

  void OnDone() {}

  template <typename Shim>
  void Init(const Shim& shim) {
    static_assert(devicetree::kIsMatcher<T>);
    shim_name_ = shim.shim_name();
    log_ = shim.log();
  }

 protected:
  // Helper for logging in to |log_|.
  void Log(const char* fmt, ...) __PRINTFLIKE(2, 3) {
    fprintf(log_, "%s: ", shim_name_);
    va_list ap;
    va_start(ap, fmt);
    vfprintf(log_, fmt, ap);
    va_end(ap);
  }

 private:
  FILE* log_;
  const char* shim_name_;
};

// Helper class for decoding interrupt cells and obtaining IRQ numbers.
class DevicetreeIrqResolver {
 public:
  struct IrqConfig {
    uint32_t irq = 0;
    uint32_t flags = 0;
  };

  constexpr DevicetreeIrqResolver() = default;
  explicit constexpr DevicetreeIrqResolver(devicetree::ByteView bytes) : interrupt_bytes_(bytes) {}

  // Attempts to either resolve |interrupt-parent| property from the |decoder| hierarchy
  // or find the |interrupt-controller| along the way.
  //
  // On success with a return value |true|, the |interrupt-controller| node has been resolved,
  // On success with a return value |false|,the |interrupt-parent| was resolved but the
  // |interrupt-controller| has not. On failure, a malformed node or property has been encountered
  // and no further actions can be performed.
  fit::result<fit::failed, bool> ResolveIrqController(const devicetree::PropertyDecoder& decoder);

  // Obtains the IRQ number from the interrupt described by the |index|-th element in the interrupt
  // property.
  IrqConfig GetIrqConfig(size_t index) const {
    ZX_ASSERT(irq_resolver_);
    const size_t entry_size = *interrupt_cells_ * sizeof(uint32_t);
    return irq_resolver_(interrupt_bytes_.subspan(index * entry_size, entry_size),
                         *interrupt_cells_)
        .value_or(IrqConfig{.irq = 0, .flags = 0});
  }

  // May only be called after resolving the IRQ Controller, see |ResolveIrqController()|.
  size_t num_entries() const {
    ZX_ASSERT(interrupt_cells_);
    return interrupt_bytes_.size() / *interrupt_cells_;
  }

  // Returns whether additional scans are required to resolve the |interrupt_parent|.
  // This is only meaningful if |ResolveIrqController| return |ScanState::kActive|.
  bool NeedsInterruptParent() const { return !error_ && interrupt_parent_ && !irq_resolver_; }

 private:
  fit::inline_function<std::optional<IrqConfig>(devicetree::ByteView, uint32_t)> irq_resolver_;
  devicetree::ByteView interrupt_bytes_;
  std::optional<uint32_t> interrupt_parent_;
  std::optional<uint32_t> interrupt_cells_;
  bool error_ = false;
};

// Decodes PSCI information from a devicetree and synthesizes a
// DRIVER_CONFIG ZBI item for it.
//
// A PSCI device is encoded within a node called "psci" with a "compatible" property
// giving its compatible PSCI revisions (i.e., values of `kCompatibleDevices` below).
//
// For example,
//
// psci {
//      compatible  = "arm,psci-0.2";
//      method      = "smc";
// };
//
// For more details please see
// https://www.kernel.org/doc/Documentation/devicetree/bindings/arm/psci.txt
class ArmDevicetreePsciItem
    : public DevicetreeItemBase<ArmDevicetreePsciItem, 1>,
      public SingleOptionalItem<zbi_dcfg_arm_psci_driver_t, ZBI_TYPE_KERNEL_DRIVER,
                                ZBI_KERNEL_DRIVER_ARM_PSCI> {
 public:
  static constexpr auto kCompatibleDevices = cpp20::to_array<std::string_view>({
      // PSCI 0.1 : Not Supported.
      // "arm,psci",
      "arm,psci-0.2",
      "arm,psci-1.0",
  });

  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);

  devicetree::ScanState OnScan() { return devicetree::ScanState::kDone; }

 private:
  devicetree::ScanState HandlePsciNode(const devicetree::NodePath& path,
                                       const devicetree::PropertyDecoder& decoder);
};

// Parses either GIC v2 or GIC v3 device node into proper ZBI item.
//
// This item will scan the devicetree for either a node compatible with GIC v2 bindings or GIC v3
// bindings. Upon finding such node it will generate either a |zbi_dcfg_arm_gic_v2_driver_t| for
// GIC v2 or a |zbi_dcfg_arm_gic_v3_driver_t| for GIC v3.
//
// In case of GIC v2, it will determine whether the MSI extension is supported or not by looking
// at the children of the GIC v2 node.
//
// Each interrupt controller contains uses a custom format for their 'reg' property, which defines
// the different address ranges required for the driver.
//
// See for GIC v2:
// * https://www.kernel.org/doc/Documentation/devicetree/bindings/interrupt-controller/arm%2Cgic.txt
// See for GIC v3:
// * https://www.kernel.org/doc/Documentation/devicetree/bindings/interrupt-controller/arm%2Cgic-v3.txt
class ArmDevicetreeGicItem
    : public DevicetreeItemBase<ArmDevicetreeGicItem, 1>,
      public SingleVariantItemBase<ArmDevicetreeGicItem, zbi_dcfg_arm_gic_v2_driver_t,
                                   zbi_dcfg_arm_gic_v3_driver_t> {
 public:
  static constexpr auto kGicV2CompatibleDevices = cpp20::to_array<std::string_view>({
      "arm,gic-400",
      "arm,cortex-a15-gic",
      "arm,cortex-a9-gic",
      "arm,cortex-a7-gic",
      "arm,arm11mp-gic",
      "brcm,brahma-b15-gic",
      "arm,arm1176jzf-devchip-gic",
      "qcom,msm-8660-qgic",
      "qcom,msm-qgic2",
  });

  static constexpr auto kGicV3CompatibleDevices = cpp20::to_array<std::string_view>({"arm,gic-v3"});

  // Boot Shim Item API.
  template <typename Shim>
  void Init(const Shim& shim) {
    DevicetreeItemBase::Init(shim);
    mmio_observer_ = &shim.mmio_observer();
  }

  // Matcher API.
  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState OnSubtree(const devicetree::NodePath& path);
  devicetree::ScanState OnScan() { return devicetree::ScanState::kDone; }

  // Boot Shim Item API.
  static constexpr zbi_header_t ItemHeader(const zbi_dcfg_arm_gic_v2_driver_t& driver) {
    return {.type = ZBI_TYPE_KERNEL_DRIVER, .extra = ZBI_KERNEL_DRIVER_ARM_GIC_V2};
  }

  static constexpr zbi_header_t ItemHeader(const zbi_dcfg_arm_gic_v3_driver_t& driver) {
    return {.type = ZBI_TYPE_KERNEL_DRIVER, .extra = ZBI_KERNEL_DRIVER_ARM_GIC_V3};
  }

 private:
  devicetree::ScanState HandleGicV2(const devicetree::NodePath& path,
                                    const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState HandleGicV3(const devicetree::NodePath& path,
                                    const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState HandleGicChildNode(const devicetree::NodePath& path,
                                           const devicetree::PropertyDecoder& decoder);

  constexpr bool IsGicChildNode() const { return gic_ != nullptr; }

  const devicetree::Node* gic_ = nullptr;
  const DevicetreeBootShimMmioObserver* mmio_observer_ = nullptr;
  bool matched_ = false;
};

// This matcher parses the 'chosen' node, which is a child of the root node('/chosen'). This node
// contains information about the commandline, ramdisk and UART.
//
// * The cmdline is contained as part of the string block of the devicetree.
//
// * The ramdisk is represented as a range in memory where the firmware loaded it, usually a ZBI.
//
// * The UART on the other hand, is represented as path(which may be aliased). Is the job of this
//   item to bootstrap the UART, which means determining which drItemiver needs to be used.
//
// For more details on the chosen node please see:
//  https://devicetree-specification.readthedocs.io/en/latest/chapter3-devicenodes.html#chosen-node
class DevicetreeChosenNodeMatcherBase
    : public DevicetreeItemBase<DevicetreeChosenNodeMatcherBase, 3> {
 public:
  DevicetreeChosenNodeMatcherBase(const char* shim_name, FILE* log)
      : DevicetreeItemBase(shim_name, log) {}

  // Matcher API.
  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState OnScan() {
    return found_chosen_ ? devicetree::ScanState::kActive : devicetree::ScanState::kDone;
  }

  // Accessors

  // Input ZBI from devicetree.
  constexpr zbitl::ByteView zbi() const { return zbi_; }

  // Command line arguments from devicetree.
  constexpr std::optional<std::string_view> cmdline() const { return cmdline_; }

  // Resolved path for stdout device(e.g. uart) from the devicetree.
  constexpr std::optional<devicetree::ResolvedPath> stdout_path() const { return resolved_stdout_; }

 protected:
  auto& uart_matcher() { return uart_matcher_; }

  auto& uart_emplacer() { return uart_emplacer_; }

 private:
  devicetree::ScanState HandleBootstrapStdout(const devicetree::NodePath& path,
                                              const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState HandleUartInterruptParent(const devicetree::PropertyDecoder& decoder);

  // May only be called after |uart_irq_.ResolveIrqController| returns |fit::ok(true)|.
  void UpdateUart() {
    if (uart_emplacer_) {
      auto irq_config = uart_irq_.GetIrqConfig(0);
      uart_dcfg_.irq = irq_config.irq;
      uart_dcfg_.flags = irq_config.flags;

      uart_emplacer_(uart_dcfg_);
    }
  }

  // Path to device node containing the stdout device (uart).
  bool found_chosen_ = false;
  std::string_view stdout_path_;
  std::optional<devicetree::ResolvedPath> resolved_stdout_;
  zbi_dcfg_simple_t uart_dcfg_ = {};

  DevicetreeIrqResolver uart_irq_;

  // Command line provided by the devicetree.
  std::string_view cmdline_;

  zbitl::ByteView zbi_;

  // Type erased match.
  fit::inline_function<bool(const devicetree::PropertyDecoder&)> uart_matcher_ = nullptr;
  fit::inline_function<void(const zbi_dcfg_simple_t&)> uart_emplacer_ = nullptr;
};

template <typename AllUartDrivers = uart::all::Driver>
class DevicetreeChosenNodeMatcher : public DevicetreeChosenNodeMatcherBase {
 public:
  DevicetreeChosenNodeMatcher(const char* shim_name, FILE* log = stdout)
      : DevicetreeChosenNodeMatcherBase(shim_name, log) {
    uart_matcher() = [this](const auto& decoder) -> bool {
      uart_emplacer() = uart_.MatchDevicetree(decoder);
      return uart_emplacer() != nullptr;
    };
  }

  // We use std::nullopt over the null driver as a clearer indication that no
  // UART was matched.
  constexpr std::optional<AllUartDrivers> TakeUart() {
    if (uart_.template holds_alternative<uart::null::Driver>()) {
      return std::nullopt;
    }
    return std::move(uart_).TakeUart();
  }

 private:
  // We use KernelDriver just for the MatchDevicetree() interface; the choice
  // of I/O provider or synchronization policy is not actually material.
  uart::all::KernelDriver<uart::BasicIoProvider, uart::UnsynchronizedPolicy, AllUartDrivers> uart_;
};

// This matcher parses 'memory' and 'reserved_memory' device nodes and 'memranges' from the
// devicetree and makes them available.
//
// The memory regions are encoded in three different sources, whose layout and number of ranges
// pero node may vary.
//  * Each 'memory' nodes defines a collection of ranges that represent ram. Memory nodes
//    are childs of the root node and contain an address as part of the name(E.g. "/memory@1234").
//  * 'reserved-memory' is a container node, whose children define collections of memory ranges
//  that should be reserved. The 'reserved-memory' node is located under the root node
//  '/reserved-memory'.
//  * 'memreseve' represents the memory reservation block, which encodes pairs describing base
//  address and length of reserved memory ranges.
//  * `ramoops` node is a child of `reserverd-memory` node which is tracked as a special range.
//  When present will eventually be used to generate `ZBI_TYPE_NVRAM`.
//
// For more information and examples of each source see :
// '/memory' :
// https://devicetree-specification.readthedocs.io/en/latest/chapter3-devicenodes.html#memory-node
// '/reserved-memory' :
// https://devicetree-specification.readthedocs.io/en/latest/chapter3-devicenodes.html#reserved-memory-node
// 'memreserve' :
// https://devicetree-specification.readthedocs.io/en/latest/chapter5-flattened-format.html#memory-reservation-block
// `ramoops`:
// https://www.kernel.org/doc/Documentation/admin-guide/ramoops.rst'
//
class DevicetreeMemoryMatcher : public DevicetreeItemBase<DevicetreeMemoryMatcher, 1> {
 public:
  // Matcher API.
  constexpr DevicetreeMemoryMatcher(const char* shim_name, FILE* log,
                                    cpp20::span<memalloc::Range> storage)
      : DevicetreeItemBase(shim_name, log), ranges_(storage) {}

  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState OnScan() { return devicetree::ScanState::kDone; }

  // Memory Item API for the bootshim to initialize the memory layout.
  // An empty set of memory ranges indicates an error while parsing the devicetree
  // memory ranges.
  constexpr cpp20::span<const memalloc::Range> ranges() const {
    if (ranges_count_ <= ranges_.size()) {
      return cpp20::span{ranges_.data(), ranges_count_};
    }
    return {};
  }

  // When valid `ramoops` node is found, then this range is filled.
  constexpr const std::optional<zbi_nvram_t>& nvram() const { return nvram_; }

 private:
  // Supports capturing `this`, a flag(whether there was an error) and the type
  // of the memory range being generated, for `AppendRangesFromReg`.
  using RangeVisitor =
      fit::inline_function<bool(uint64_t address, uint64_t size), 4 * sizeof(void*)>;

  // Append special ranges to the memory regions. This will be used later for
  // initializing the pool allocation memory.
  constexpr bool AppendRange(const memalloc::Range& range) {
    if (ranges_count_ >= ranges_.size()) {
      if (ranges_count_ == ranges_.size()) {
        OnError("Not enough preallocated ranges.");
      }
      ranges_count_ = ranges_.size() + 1;
      return false;
    }
    ranges_[ranges_count_++] = range;
    return true;
  }

  bool AppendRangesFromReg(const devicetree::PropertyDecoder& decoder,
                           memalloc::Type memrange_type);

  void OnEachRangeFromReg(const devicetree::PropertyDecoder& decoder, RangeVisitor range_visitor);

  devicetree::ScanState HandleMemoryNode(const devicetree::NodePath& path,

                                         const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState HandleReservedMemoryNode(const devicetree::NodePath& path,
                                                 const devicetree::PropertyDecoder& decoder);

  cpp20::span<memalloc::Range> ranges_;
  size_t ranges_count_ = 0;
  std::optional<zbi_nvram_t> nvram_;
};

// This routine passes each memory reservation to a provided callback,
// excluding the ranges that overlap with a select number of "exclusions". The
// The callback should return `true` if it wishes to proceed with the iteration
// and `false` if it wishes to short-circuit; in the latter case the routine
// itself will return `false`. The exclusions should be non-overlapping and in
// order.
//
// While contrary to the devicetree spec - which says that a memory reservation
// is memory that should not be used by the kernel - we have encountered
// bootloaders that do generate reservations for the devicetree blob and
// ramdisk. This routine works around that to ensure that such ranges do not
// end up accounted for as RESERVED.
//
// This logic is separate from any matcher as memory reservations are not
// encoded within a devicetree blob's tree structure, and the ramdisk - one of
// the intended exclusions - is the product itself of a 'chosen' matcher.
template <typename Callback>
bool ForEachDevicetreeMemoryReservation(const devicetree::Devicetree& fdt,
                                        cpp20::span<const memalloc::Range> exclusions,
                                        Callback&& cb) {
  using Reservation = devicetree::MemoryReservation;
  static_assert(std::is_invocable_r_v<bool, Callback, Reservation>);

  ZX_ASSERT(std::is_sorted(exclusions.begin(), exclusions.end(), [](auto a, auto b) {
    return (a.addr < b.addr) || (a.addr == b.addr && a.size < b.size);
  }));
  for (size_t i = 0; i + 1 < exclusions.size(); ++i) {
    ZX_ASSERT_MSG(exclusions[i].end() <= exclusions[i + 1].addr,
                  "Overlapping memory reservation exclusions: [%#" PRIx64 ", %#" PRIx64
                  "), [%#" PRIx64 ", %#" PRIx64 ")",                 //
                  exclusions[i].addr, exclusions[i].end(),           //
                  exclusions[i + 1].addr, exclusions[i + 1].end());  //
  }

  auto filter_exclusions = [&](Reservation res) -> bool {
    for (auto exclusion : exclusions) {
      //              [ res )
      // [ exclusion ) ...
      if (exclusion.end() <= res.start) {
        continue;
      }
      // [ res )
      //         [ exclusion ) ...
      if (res.end() <= exclusion.addr) {
        return cb(res);
      }

      // [ res )
      //     [ exclusion ) ...
      //
      // or
      //
      // [        res        )
      //     [ exclusion ) ...
      if (res.start < exclusion.addr) {
        if (!cb(Reservation{
                .start = res.start,
                .size = exclusion.addr - res.start,
            })) {
          return false;
        }
      }
      if (res.end() <= exclusion.end()) {  // First case.
        return true;
      }
      res = {.start = exclusion.end(), .size = res.end() - exclusion.end()};
    }
    return cb(res);
  };
  for (auto res : fdt.memory_reservations()) {
    if (!filter_exclusions(res)) {
      return false;
    }
  }
  return true;
}

// This item parses the '/cpus' 'timebase-frequency property to generate a timer driver
// configuration ZBI item.
//
// The timebase frequency specifies the clock frequency of the RISC-V timer device.
//
// See:
// https://www.kernel.org/doc/Documentation/devicetree/bindings/timer/riscv%2Ctimer.yaml
class RiscvDevicetreeTimerItem
    : public DevicetreeItemBase<RiscvDevicetreeTimerItem, 1>,
      public SingleOptionalItem<zbi_dcfg_riscv_generic_timer_driver_t, ZBI_TYPE_KERNEL_DRIVER,
                                ZBI_KERNEL_DRIVER_RISCV_GENERIC_TIMER> {
 public:
  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState OnScan() { return devicetree::ScanState::kDone; }
};

// Parses interrupt controller node that is compatible with PLIC (Platform Level Interrupt
// Controller bindings. For the time being, it only parses the mmio base for the plic register bank
// and the number of IRQs. Until the zbi item representing the riscv PLIC is extended to represent
// the contexts(hart_id, priority), the 'interrupt-extended' property is not yet decoded.
//
// See:
// https://www.kernel.org/doc/Documentation/devicetree/bindings/interrupt-controller/sifive%2Cplic-1.0.0.txt
class RiscvDevicetreePlicItem
    : public DevicetreeItemBase<RiscvDevicetreePlicItem, 1>,
      public SingleOptionalItem<zbi_dcfg_riscv_plic_driver_t, ZBI_TYPE_KERNEL_DRIVER,
                                ZBI_KERNEL_DRIVER_RISCV_PLIC> {
 public:
  static constexpr auto kCompatibleDevices = cpp20::to_array({"sifive,plic-1.0.0", "riscv,plic0"});

  // Matcher API.
  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState OnScan() { return devicetree::ScanState::kDone; }

 private:
  devicetree::ScanState HandlePlicNode(const devicetree::NodePath& path,
                                       const devicetree::PropertyDecoder& decoder);
};

// Parses '/cpus' node to generate |ZBI_TYPE_CPU_TOPOLOGY| item. This involves both parsing CPU
// nodes and the '/cpus/cpu-map' node when present. Lack of a 'cpu-map' means all nodes are
// considered siblings which is reflected with none of them having a parent.
//
// A cluster's performance class is the normalized capacity of a cluster based on the maximum
// capacity of all clusters.
//
// cluster-performance-class[i] = cluster-capacity[i] * 255 / max(cluster-capacity[0]....N)
//
// When a cluster-capacity is not able to be determined because no property in the node provides
// this value then all clusters are given a performance class of 1. Its important to realize that
// the actual value of the performance class is only a representative of the relative difference
// between difference clusters.
//
// See:
// https://www.kernel.org/doc/Documentation/devicetree/bindings/arm/cpu-capacity.txt
// https://www.kernel.org/doc/Documentation/devicetree/bindings/cpu/cpu-topology.txt
class DevicetreeCpuTopologyItem : public DevicetreeItemBase<DevicetreeCpuTopologyItem, 2>,
                                  public ItemBase {
 public:
  // Matcher API.
  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState OnSubtree(const devicetree::NodePath& path);
  devicetree::ScanState OnScan() {
    return found_cpus_ ? devicetree::ScanState::kActive : devicetree::ScanState::kDone;
  }

  // Finalizes cpu_entries() (excluding skipped entries due to malformed
  // fields) and sorts them by ID for normalization's sake, which
  // is convenient at the very least for test purposes.
  void OnDone();

  size_t size_bytes() const { return ItemSize(node_element_count() * sizeof(zbi_topology_node_t)); }

  fit::result<DataZbi::Error> AppendItems(DataZbi& zbi) const;

 protected:
  // Used for decoding CPU-related properties.
  struct CpuEntry {
    std::optional<uint32_t> phandle;
    std::optional<devicetree::RegProperty> reg;
    devicetree::Properties properties;
  };

  cpp20::span<const CpuEntry> cpu_entries() const { return cpu_entries_; }

  // Callback used during matching for checking architecture-specific processor
  // information. The returned boolean indicates whether matching should
  // record the current CPU, false indicating that crucial information was
  // missing or malformed. For finer-grained reporting, the expectation is that
  // `this` can be captured to leverage the devicetree item's logging
  // facilities.
  using CheckArchCpuInfo = fit::inline_function<bool(const CpuEntry& entry)>;

  // Callback used during AppendItems() for setting architecture-specific
  // processor information.
  using SetArchCpuInfo =
      fit::inline_function<void(zbi_topology_processor_t&, const CpuEntry& entry)>;

  template <typename Shim>
  void Init(const Shim& shim, CheckArchCpuInfo arch_info_checker, SetArchCpuInfo arch_info_setter) {
    DevicetreeItemBase<DevicetreeCpuTopologyItem, 2>::Init(shim);
    allocator_ = &shim.allocator();
    arch_info_checker_ = std::move(arch_info_checker);
    arch_info_setter_ = std::move(arch_info_setter);
  }

  template <typename T>
  cpp20::span<T> Allocate(
      size_t count, fbl::AllocChecker& ac,
      cpp20::source_location location = cpp20::source_location::current()) const {
    size_t alloc_size = sizeof(T) * count;
    auto* alloc = static_cast<T*>((*allocator_)(alloc_size, alignof(T), ac));
    if (!alloc) {
      // Log allocation failure. The effect is that the matcher will keep looking and will fail to
      // make progress. But the error will be logged.
      auto* self = const_cast<DevicetreeCpuTopologyItem*>(this);
      self->OnError("Allocation Failed.");
      self->Log("at %s:%u\n", location.file_name(), static_cast<unsigned int>(location.line()));
      count = 0;
    }
    memset(alloc, 0, alloc_size);
    return cpp20::span<T>(alloc, count);
  }

  template <typename T>
  T* Allocate(fbl::AllocChecker& ac,
              cpp20::source_location location = cpp20::source_location::current()) const {
    return Allocate<T>(1, ac, location).data();
  }

 private:
  // Devicetree 'cpu-map' entities.
  enum class TopologyEntryType {
    kSocket,
    kCluster,
    kCore,
    kThread,
  };

  // Generic entry in the devicetree, maintains parent relationship and a view into the properties.
  struct CpuMapEntry {
    // Type of the entry.
    TopologyEntryType type;
    // Index of the parent entry on the cpu map.
    size_t parent_index;
    // Index of the cluster entry where this node is contained within the cpu map.
    std::optional<uint32_t> cluster_index;
    // 'phandle' obtained from the 'core' or 'thread' entries. Nodes containing this 'phandle'
    // represent a processing unit, and are leaf nodes in the cpu map.
    std::optional<uint32_t> cpu_phandle;
    // Index of the |CpuEntry| in the |cpus_| representing the resolved link of the |cpu_phandle|
    // to a |cpu| node.
    std::optional<uint32_t> cpu_index;
    // Index of |zbi_topology_node_t| in the |ZBI_ITEM_TYPE_CPU_TOPOLOGY| that was generated from
    // this |CpuMapEntry|.
    std::optional<size_t> topology_node_index;
  };

  // May only be called after |Init| and a full match sequence has been performed.
  constexpr size_t node_element_count() const { return topology_node_count_; }

  devicetree::ScanState IncreaseEntryNodeCountFirstScan(const devicetree::NodePath& path,
                                                        const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState AddEntryNodeSecondScan(const devicetree::NodePath& path,
                                               const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState IncreaseCpuNodeCountFirstScan(const devicetree::NodePath& path,
                                                      const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState AddCpuNodeSecondScan(const devicetree::NodePath& path,
                                             const devicetree::PropertyDecoder& decoder);

  static constexpr bool IsCpuMapNode(std::string_view node_name, std::string_view prefix) {
    if (!cpp20::starts_with(node_name, prefix)) {
      return false;
    }
    // Must match prefix[0-9].
    return node_name.substr(prefix.length()).find_first_not_of("01234567890") ==
           std::string_view::npos;
  }

  // After both |entries_| and |cpus_| have been filled this routine will fill up
  // the reference from an entry to a 'cpu' node.
  fit::result<ItemBase::DataZbi::Error> UpdateEntryCpuLinks() const;

  // Recalculates performance class based on CPU capacity related properties.
  fit::result<ItemBase::DataZbi::Error> CalculateClusterPerformanceClass(
      cpp20::span<zbi_topology_node_t> nodes) const;

  // Flattened 'cpu-map'.
  cpp20::span<CpuMapEntry> map_entries_;
  uint32_t map_entry_index_ = 0;
  uint32_t map_entry_count_ = 0;
  bool has_cpu_map_ = false;

  // Used to track parent-child relationships when building the flattened cpu-map.
  std::optional<uint32_t> current_socket_;
  std::optional<uint32_t> current_cluster_;
  std::optional<uint32_t> current_core_;

  cpp20::span<CpuEntry> cpu_entries_;
  uint32_t cpu_entry_count_ = 0;
  uint32_t cpu_entry_index_ = 0;
  uint32_t cluster_count_ = 0;

  size_t topology_node_count_ = 0;

  // Allocation is environment specific, so we delegate that to a lambda.
  mutable const DevicetreeBootShimAllocator* allocator_ = nullptr;

  CheckArchCpuInfo arch_info_checker_;
  SetArchCpuInfo arch_info_setter_;
  bool found_cpus_ = false;
};

class RiscvDevicetreeCpuTopologyItemBase : public DevicetreeCpuTopologyItem,
                                           public SingleItem<ZBI_TYPE_RISCV64_ISA_STRTAB> {
 public:
  explicit RiscvDevicetreeCpuTopologyItemBase(uint64_t boot_hart_id)
      : boot_hart_id_(boot_hart_id) {}

  ~RiscvDevicetreeCpuTopologyItemBase() { id_to_index_.clear_unsafe(); }

  template <typename Shim>
  void Init(Shim& shim) {
    DevicetreeCpuTopologyItem::Init(
        shim,  //
        [this](const CpuEntry& cpu_entry) {
          // The presence of "reg" should already have been validated.
          std::optional<devicetree::RegProperty> reg = cpu_entry.reg;
          ZX_DEBUG_ASSERT(reg);
          if (reg->size() != 1) {
            OnError("'reg' property in 'cpu' node contains an unexpected number of cells.");
            return false;
          }
          if (!(*reg)[0].address()) {
            OnError("Could not parse first cell of 'reg' property in 'cpu' node.");
            return false;
          }

          // No "riscv,isa"-less CPU should be recorded.
          devicetree::PropertyDecoder decoder(cpu_entry.properties);
          auto isa_string =
              decoder.FindAndDecodeProperty<&devicetree::PropertyValue::AsString>("riscv,isa");
          if (!isa_string) {
            OnError("Missing \"riscv,isa\" property");
            return false;
          }
          return true;
        },
        [this](zbi_topology_processor_t& node, const CpuEntry& cpu_entry) -> void {
          node.architecture_info.discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_RISCV64;

          auto reg = cpu_entry.reg;
          ZX_DEBUG_ASSERT(reg);
          std::optional<uint64_t> hart_id = (*reg)[0].address();
          ZX_DEBUG_ASSERT(hart_id);
          node.architecture_info.riscv64.hart_id = *hart_id;

          if (*hart_id == boot_hart_id_) {
            node.flags |= ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY;
          } else {
            node.flags &= ~ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY;
          }
          auto it = id_to_index_.find(*hart_id);
          if (it != id_to_index_.end()) {
            node.architecture_info.riscv64.isa_strtab_index =
                static_cast<uint16_t>(it->strtab_index);
          }
        });
  }

  size_t size_bytes() const {
    return DevicetreeCpuTopologyItem::size_bytes() + IsaStrtabItem::size_bytes();
  }

  // Finalizes the ISA string table.
  void OnDone();

  fit::result<DataZbi::Error> AppendItems(DataZbi& zbi) const {
    if (auto result = DevicetreeCpuTopologyItem::AppendItems(zbi); result.is_error()) {
      return result;
    }
    return IsaStrtabItem::AppendItems(zbi);
  }

 private:
  using IsaStrtabItem = SingleItem<ZBI_TYPE_RISCV64_ISA_STRTAB>;

  struct IsaStrtabIndex
      : public fbl::SinglyLinkedListable<IsaStrtabIndex*, fbl::NodeOptions::AllowClearUnsafe> {
    // Required to instantiate fbl::DefaultKeyedObjectTraits.
    uint64_t GetKey() const { return hart_id; }

    // Required to instantiate fbl::DefaultHashTraits.
    static size_t GetHash(uint64_t key) { return static_cast<size_t>(key); }

    uint64_t hart_id = 0;
    size_t strtab_index = 0;
  };

  uint64_t boot_hart_id_;
  cpp20::span<char> isa_strtab_;
  fbl::HashTable<uint64_t, IsaStrtabIndex*> id_to_index_;
};

// BootHartIdGetter provides a static means of accessing the boot hart ID, which
// derives from outside of the devicetree: it must provide a method of the form
// `static uint64_t Get()`.
template <typename BootHartIdGetter>
class RiscvDevicetreeCpuTopologyItem : public RiscvDevicetreeCpuTopologyItemBase {
 public:
  RiscvDevicetreeCpuTopologyItem() : RiscvDevicetreeCpuTopologyItemBase(BootHartIdGetter::Get()) {}
};

class ArmDevicetreeCpuTopologyItem : public DevicetreeCpuTopologyItem {
 public:
  template <typename Shim>
  void Init(Shim& shim) {
    DevicetreeCpuTopologyItem::Init(
        shim,  //
        [this](const CpuEntry& cpu_entry) {
          // The presence of "reg" should already have been validated.
          std::optional<devicetree::RegProperty> reg = cpu_entry.reg;
          ZX_DEBUG_ASSERT(reg);
          if (reg->size() == 0 || reg->size() > 2) {
            OnError("'reg' property in 'cpu' node contains an unexpected number of cells.");
            return false;
          }
          if (!(*reg)[0].address()) {
            OnError("Could not parse first cell of 'reg' property in 'cpu' node.");
            return false;
          }
          if (reg->size() == 2 && !(*reg)[1].address()) {
            OnError("Could not parse second cell of 'reg' property in 'cpu' node.");
            return false;
          }
          return true;
        },
        [](zbi_topology_processor_t& node, const CpuEntry& cpu_entry) {
          node.architecture_info.discriminant = ZBI_TOPOLOGY_ARCHITECTURE_INFO_ARM64;
          devicetree::PropertyDecoder decoder(cpu_entry.properties);

          // Even though the decoded "reg" property is readily available as
          // cpu_entry.reg, it's more convenient to normalize it as an array of
          // with elements containing only a single address cell, rather than
          // conditionally dealing with the case with elements with two address
          // cells.
          //
          // Validations of debug asserts below were made in the arch info
          // checker.
          auto reg_prop = decoder.FindProperty("reg");
          ZX_DEBUG_ASSERT(reg_prop);
          auto reg = devicetree::RegProperty::Create(1, 0, reg_prop->AsBytes());
          ZX_DEBUG_ASSERT(reg);
          ZX_DEBUG_ASSERT(reg->size() == 1 || reg->size() == 2);

          auto set_affs = [&node](uint64_t cell) {
            // AFF 0
            node.architecture_info.arm64.cpu_id = cell & 0xff;
            // AFF 1
            node.architecture_info.arm64.cluster_1_id = (cell >> 8) & 0xff;
            // AFF 2
            node.architecture_info.arm64.cluster_2_id = (cell >> 16) & 0xff;
          };

          auto set_boot_cpu = [&node]() {
            // Look for MPIDR 0.
            const auto& arch_info = node.architecture_info.arm64;
            if (arch_info.cpu_id == 0 && arch_info.cluster_1_id == 0 &&
                arch_info.cluster_2_id == 0 && arch_info.cluster_3_id == 0) {
              node.flags |= ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY;
            } else {
              node.flags &= ~ZBI_TOPOLOGY_PROCESSOR_FLAGS_PRIMARY;
            }
          };

          auto cell_0 = (*reg)[0].address();
          ZX_DEBUG_ASSERT(cell_0);  // Validated in the arch info checker.

          node.architecture_info.arm64.gic_id =
              static_cast<uint8_t>(node.logical_ids[node.logical_id_count - 1]);

          // One cell.
          // The reg cell bits [23:0] must be set to bits [23:0] of MPIDR_EL1.
          if (reg->size() == 1) {
            set_affs(*cell_0);
            node.architecture_info.arm64.cluster_3_id = 0;
            set_boot_cpu();
            return;
          }

          // Two cells.
          // The first reg cell bits [7:0] must be set to  bits [39:32] of MPIDR_EL1.
          // The second reg cell bits [23:0] must be set to bits [23:0] of MPIDR_EL1.
          auto cell_1 = (*reg)[1].address();
          ZX_DEBUG_ASSERT(cell_1);  // Validated in the arch info checker.

          set_affs(*cell_1);
          node.architecture_info.arm64.cluster_3_id = *cell_0 & 0xFF;
          set_boot_cpu();
        });
  }
};

// See https://www.kernel.org/doc/Documentation/devicetree/bindings/arm/arch_timer.txt
class ArmDevicetreeTimerItem
    : public DevicetreeItemBase<ArmDevicetreeTimerItem, 2>,
      public SingleOptionalItem<zbi_dcfg_arm_generic_timer_driver_t, ZBI_TYPE_KERNEL_DRIVER,
                                ZBI_KERNEL_DRIVER_ARM_GENERIC_TIMER> {
 public:
  static constexpr auto kCompatibleDevices =
      cpp20::to_array({"arm,armv7-timer", "arm,armv8-timer"});

  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder);
  devicetree::ScanState OnScan() {
    return found_timer_ ? devicetree::ScanState::kActive : devicetree::ScanState::kDone;
  }

 private:
  bool found_timer_ = false;
  DevicetreeIrqResolver irq_;
  // Optional, maps to frequency override.
  std::optional<uint64_t> frequency_;
};

// A flat Devicetree ZBI Item.
using DevicetreeDtbItem = SingleItem<ZBI_TYPE_DEVICETREE>;

// Define an item that maps an arbitrary set of `Watchdogs` to `zbi_dcfg_generic32_watchdog_t`, that
// is a generic representation of a watchdog driver described by a set of actions that are mapped to
// a sequence of R-M-W operations in specific MMIO regions.
//
// Specifically given the set of `Wacthdogs` the first non-disabled matching device node will be
// used to fill the payload.
//
// See '<lib/boot-shim/watchdog.h>' for Watchdog Item API contract.
template <class... Watchdogs>
class GenericWatchdogItemBase
    : public DevicetreeItemBase<GenericWatchdogItemBase<Watchdogs...>, 1>,
      public SingleOptionalItem<zbi_dcfg_generic32_watchdog_t, ZBI_TYPE_KERNEL_DRIVER,
                                ZBI_KERNEL_DRIVER_GENERIC32_WATCHDOG> {
 public:
  devicetree::ScanState OnNode(const devicetree::NodePath& path,
                               const devicetree::PropertyDecoder& decoder) {
    auto compatibles =
        decoder.FindAndDecodeProperty<&devicetree::PropertyValue::AsStringList>("compatible");
    if (!compatibles) {
      return devicetree::ScanState::kActive;
    }

    auto device_status =
        decoder.FindAndDecodeProperty<&devicetree::PropertyValue::AsString>("status");
    // Ignore disabled device nodes and keep looking.
    if (device_status && device_status.value() == "disabled") {
      return devicetree::ScanState::kActive;
    }

    for (auto compatible : *compatibles) {
      auto result = (Match<Watchdogs>(compatible, decoder) || ...);
      // Matched and maybe filled the payload.
      if (result) {
        return devicetree::ScanState::kDone;
      }
    }
    return devicetree::ScanState::kActive;
  }

  // If after a full scan no watchdog is found, we should end.
  devicetree::ScanState OnScan() { return devicetree::ScanState::kDone; }

 private:
  template <class Watchdog>
  bool Match(std::string_view compatible, const devicetree::PropertyDecoder& decoder) {
    for (std::string_view device_compatible : Watchdog::kCompatibleDevices) {
      if (device_compatible == compatible) {
        if (auto payload = Watchdog::MaybeCreate(decoder); payload) {
          set_payload(*payload);
        }
        return true;
      }
    }
    return false;
  }
};

using GenericWatchdogItem = WithAllWatchdogs<GenericWatchdogItemBase>;
using NvramItem = SingleOptionalItem<zbi_nvram_t, ZBI_TYPE_NVRAM>;

}  // namespace boot_shim

#endif  // ZIRCON_KERNEL_PHYS_LIB_BOOT_SHIM_INCLUDE_LIB_BOOT_SHIM_DEVICETREE_H_
