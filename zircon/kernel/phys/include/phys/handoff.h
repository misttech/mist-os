// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_HANDOFF_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_HANDOFF_H_

// Note: we refrain from using the ktl namespace as <phys/handoff.h> is
// expected to be compiled in the userboot toolchain.

#include <lib/arch/ticks.h>
#include <lib/crypto/entropy_pool.h>
#include <lib/memalloc/range.h>
#include <lib/stdcompat/span.h>
#include <lib/uart/all.h>
#include <lib/zbi-format/board.h>
#include <lib/zbi-format/cpu.h>
#include <lib/zbi-format/memory.h>
#include <lib/zbi-format/reboot.h>
#include <lib/zbi-format/zbi.h>
#include <stddef.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <array>
#include <optional>
#include <string_view>
#include <type_traits>

#include <phys/arch/arch-handoff.h>

#include "handoff-ptr.h"

struct BootOptions;

// This holds arch::EarlyTicks timestamps collected by physboot before the
// kernel proper is cognizant.  Once the platform timer hardware is set up for
// real, platform_convert_early_ticks translates these values into zx_instant_mono_ticks_t
// values that can be published as kcounters and then converted to actual time
// units in userland via zx_ticks_per_second().
//
// platform_convert_early_ticks returns zero if arch::EarlyTicks samples cannot
// be accurately converted to zx_instant_mono_ticks_t.  This can happen on suboptimal x86
// hardware, where the early samples are in TSC but the platform timer decides
// that a synchronized and monotonic TSC is not available on the machine.
class PhysBootTimes {
 public:
  // These are various time points sampled during physboot's work.
  // kernel/top/handoff.cc has a kcounter corresponding to each of these.
  // When a new time point is added here, a new kcounter must be added
  // there to make that sample visible anywhere.
  enum Index : size_t {
    kZbiEntry,         // ZBI entry from boot loader.
    kPhysSetup,        // Earliest/arch-specific phys setup (e.g. paging).
    kDecompressStart,  // Begin decompression.
    kDecompressEnd,    // STORAGE_KERNEL decompressed.
    kZbiDone,          // ZBI items have been ingested.
    kCount
  };

  constexpr arch::EarlyTicks Get(Index i) const { return timestamps_[i]; }

  constexpr void Set(Index i, arch::EarlyTicks ts) { timestamps_[i] = ts; }

  void SampleNow(Index i) { Set(i, arch::EarlyTicks::Get()); }

 private:
  arch::EarlyTicks timestamps_[kCount] = {};
};

// VMOs to publish as is.
struct PhysVmo {
  using Name = std::array<char, ZX_MAX_NAME_LEN>;

  // The maximum number of additional VMOs expected to be in the hand-off
  // beyond the special ones explicitly enumerated.
  static constexpr size_t kMaxExtraHandoffPhysVmos = 3;

  // The full page-aligned size of the memory.
  constexpr size_t size_bytes() const { return (content_size + ZX_PAGE_SIZE - 1) & -ZX_PAGE_SIZE; }

  void set_name(std::string_view new_name) { new_name.copy(name.data(), name.size() - 1); }

  // The physical address of the memory.
  uintptr_t addr = 0;
  size_t content_size = 0;
  Name name{};
};
static_assert(std::is_default_constructible_v<PhysVmo>);

// This holds (or points to) everything that is handed off from physboot to the
// kernel proper at boot time.
struct PhysHandoff {
  // Whether the given type represents physical memory that should be turned
  // into a VMO.
  static bool IsPhysVmoType(memalloc::Type type) {
    switch (type) {
      case memalloc::Type::kDataZbi:
      case memalloc::Type::kPhysDebugdata:
      case memalloc::Type::kPhysLog:
      case memalloc::Type::kUserboot:
      case memalloc::Type::kVdso:
        return true;
      default:
        break;
    }
    return false;
  }

  constexpr bool Valid() const { return magic == kMagic; }

  static constexpr uint64_t kMagic = 0xfeedfaceb002da2a;

  const uint64_t magic = kMagic;

  // TODO(https://fxbug.dev/42164859): This will eventually be made a permanent pointer.
  PhysHandoffTemporaryPtr<const BootOptions> boot_options;

  PhysBootTimes times;
  static_assert(std::is_default_constructible_v<PhysBootTimes>);

  // TODO(https://fxbug.dev/42164859): This will eventually be made a permanent pointer.
  PhysHandoffTemporaryString version_string;

  // Additional VMOs to be published to userland as-is and not otherwise used by
  // the kernel proper.
  PhysHandoffTemporarySpan<const PhysVmo> extra_vmos;

  // The data ZBI.
  PhysVmo zbi;

  // The vDSO.
  PhysVmo vdso;

  // Userboot.
  PhysVmo userboot;

  // Entropy gleaned from ZBI Items such as 'ZBI_TYPE_SECURE_ENTROPY' and/or command line.
  std::optional<crypto::EntropyPool> entropy_pool;

  // ZBI container of items to be propagated in mexec.
  // TODO(https://fxbug.dev/42164859): later this will be propagated
  // as a whole page the kernel can stuff into a VMO.
  PhysHandoffTemporarySpan<const std::byte> mexec_data;

  // Architecture-specific content.
  ArchPhysHandoff arch_handoff;
  static_assert(std::is_default_constructible_v<ArchPhysHandoff>);

  // A normalized accounting of RAM (and peripheral ranges). It consists of
  // ranges that are maximally contiguous and in sorted order, and features
  // allocations that are of interest to the kernel.
  PhysHandoffTemporarySpan<const memalloc::Range> memory;

  // ZBI_TYPE_CPU_TOPOLOGY payload (or translated legacy equivalents).
  PhysHandoffTemporarySpan<const zbi_topology_node_t> cpu_topology;

  // ZBI_TYPE_CRASHLOG payload.
  PhysHandoffTemporaryString crashlog;

  // ZBI_TYPE_HW_REBOOT_REASON payload.
  std::optional<zbi_hw_reboot_reason_t> reboot_reason;

  // ZBI_TYPE_NVRAM payload.
  // A physical memory region that will persist across warm boots.
  std::optional<zbi_nvram_t> nvram;

  // ZBI_TYPE_PLATFORM_ID payload.
  std::optional<zbi_platform_id_t> platform_id;

  // ZBI_TYPE_ACPI_RSDP payload.
  // Physical address of the ACPI RSDP (Root System Descriptor Pointer).
  std::optional<uint64_t> acpi_rsdp;

  // ZBI_TYPE_SMBIOS payload.
  // Physical address of the SMBIOS tables.
  std::optional<uint64_t> smbios_phys;

  // ZBI_TYPE_EFI_MEMORY_ATTRIBUTES_TABLE payload.
  // EFI memory attributes table.
  PhysHandoffTemporarySpan<const std::byte> efi_memory_attributes;

  // ZBI_TYPE_EFI_SYSTEM_TABLE payload.
  // Physical address of the EFI system table.
  std::optional<uint64_t> efi_system_table;
};

static_assert(std::is_default_constructible_v<PhysHandoff>);

extern PhysHandoff* gPhysHandoff;

// This is the entry point function for the ELF kernel.
extern "C" [[noreturn]] void PhysbootHandoff(PhysHandoff* handoff);

#ifdef _KERNEL

// These functions relate to PhysHandoff but exist only in the kernel proper.

#include <stddef.h>

#include <fbl/ref_ptr.h>
#include <object/handle.h>

// Forward declaration; defined in <vm/vm_object.h>
class VmObject;

// Called as soon as the physmap is available to set the gPhysHandoff pointer.
void HandoffFromPhys(paddr_t handoff_paddr);

// This can be used after HandoffFromPhys and before the ZBI is handed off to
// userboot at the very end of kernel initialization code.  Userboot calls it
// with true to ensure no later calls will succeed.

// The remaining hand-off data to be consumed at the end of the hand-off phase
// (see EndHandoff()).
struct HandoffEnd {
  // The data ZBI.
  HandleOwner zbi;

  fbl::RefPtr<VmObject> vdso;
  fbl::RefPtr<VmObject> userboot;

  // The VMOs deriving from the phys environment. As returned by EndHandoff(),
  // the entirety of the array will be populated by real handles (if only by
  // stub VMOs) (as is convenient for userboot, its intended caller).
  std::array<HandleOwner, PhysVmo::kMaxExtraHandoffPhysVmos> extra_phys_vmos;
};

// Formally ends the hand-off phase, unsetting gPhysHandoff and returning the
// remaining hand-off data left to be consumed (in a userboot-friendly way), and
// freeing temporary hand-off memory (see PhysHandoff::temporary_memory).
//
// After the end of hand-off, all pointers previously referenced by gPhysHandoff
// should be regarded as freed and unusable.
HandoffEnd EndHandoff();

#endif  // _KERNEL

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_HANDOFF_H_
