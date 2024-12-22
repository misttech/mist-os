// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2009 Corey Tabaka
// Copyright (c) 2015 Intel Corporation
// Copyright (c) 2016 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <assert.h>
#include <lib/boot-options/boot-options.h>
#include <lib/cksum.h>
#include <lib/debuglog.h>
#include <lib/lazy_init/lazy_init.h>
#include <lib/memalloc/range.h>
#include <lib/system-topology.h>
#include <lib/zbi-format/cpu.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zircon-internal/macros.h>
#include <mexec.h>
#include <platform.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstddef>

#include <arch/mp.h>
#include <arch/ops.h>
#include <arch/x86.h>
#include <arch/x86/apic.h>
#include <arch/x86/mmu.h>
#include <arch/x86/pv.h>
#include <explicit-memory/bytes.h>
#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <fbl/vector.h>
#include <kernel/cpu.h>
#include <kernel/cpu_distance_map.h>
#include <ktl/algorithm.h>
#include <lk/init.h>
#include <phys/handoff.h>
#include <platform/console.h>
#include <platform/crashlog.h>
#include <platform/efi.h>
#include <platform/efi_crashlog.h>
#include <platform/keyboard.h>
#include <platform/pc.h>
#include <platform/pc/acpi.h>
#include <platform/pc/bootloader.h>
#include <platform/pc/smbios.h>
#include <platform/ram_mappable_crashlog.h>
#include <vm/physmap.h>
#include <vm/pmm.h>
#include <vm/vm_aspace.h>

#include "platform_p.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

pc_bootloader_info_t bootloader;

namespace {
namespace crashlog_impls {
lazy_init::LazyInit<RamMappableCrashlog, lazy_init::CheckType::None,
                    lazy_init::Destructor::Disabled>
    ram_mappable;
EfiCrashlog efi;
}  // namespace crashlog_impls
}  // namespace

static void platform_save_bootloader_data(void) {
  if (gPhysHandoff->arch_handoff.framebuffer) {
    bootloader.fb = gPhysHandoff->arch_handoff.framebuffer.value();
  }

  // Record any previous crashlog.
  if (ktl::string_view crashlog = gPhysHandoff->crashlog.get(); !crashlog.empty()) {
    crashlog_impls::efi.SetLastCrashlogLocation(crashlog);
  }

  // If we have an NVRAM location and we have not already configured a platform
  // crashlog implementation, use the NVRAM location to back a
  // RamMappableCrashlog implementation and configure the generic platform
  // layer to use it.
  if (gPhysHandoff->nvram && !PlatformCrashlog::HasNonTrivialImpl()) {
    const zbi_nvram_t& nvram = gPhysHandoff->nvram.value();
    crashlog_impls::ram_mappable.Initialize(nvram.base, nvram.length);
    PlatformCrashlog::Bind(crashlog_impls::ram_mappable.Get());
  }
}

static void platform_init_crashlog(void) {
  // Nothing to do if we have already selected a crashlog implementation.
  if (PlatformCrashlog::HasNonTrivialImpl()) {
    return;
  }

  // Initialize and select the EfiCrashlog implementation.
  PlatformCrashlog::Bind(crashlog_impls::efi);
}

// Number of pages required to identity map 16GiB of memory.
constexpr size_t kBytesToIdentityMap = 16ull * GB;
constexpr size_t kNumL2PageTables = kBytesToIdentityMap / (2ull * MB * NO_OF_PT_ENTRIES);
constexpr size_t kNumL3PageTables = 1;
constexpr size_t kNumL4PageTables = 1;
constexpr size_t kTotalPageTableCount = kNumL2PageTables + kNumL3PageTables + kNumL4PageTables;

static fbl::RefPtr<VmAspace> mexec_identity_aspace;

// Array of pages that are safe to use for the new kernel's page tables.  These must
// be after where the new boot image will be placed during mexec.  This array is
// populated in platform_mexec_prep and used in platform_mexec.
static paddr_t mexec_safe_pages[kTotalPageTableCount];

void platform_mexec_prep(uintptr_t final_bootimage_addr, size_t final_bootimage_len) {
  DEBUG_ASSERT(!arch_ints_disabled());
  DEBUG_ASSERT(mp_get_online_mask() == cpu_num_to_mask(BOOT_CPU_ID));

  // This code only handles one L3 and one L4 page table for now. Fail if
  // there are more L2 page tables than can fit in one L3 page table.
  static_assert(kNumL2PageTables <= NO_OF_PT_ENTRIES,
                "Kexec identity map size is too large. Only one L3 PTE is supported at this time.");
  static_assert(kNumL3PageTables == 1, "Only 1 L3 page table is supported at this time.");
  static_assert(kNumL4PageTables == 1, "Only 1 L4 page table is supported at this time.");

  // Identity map the first 16GiB of RAM
  mexec_identity_aspace = VmAspace::Create(VmAspace::Type::LowKernel, "x86-64 mexec 1:1");
  DEBUG_ASSERT(mexec_identity_aspace);

  const uint perm_flags_rwx =
      ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE | ARCH_MMU_FLAG_PERM_EXECUTE;
  void* identity_address = 0x0;
  paddr_t pa = 0;
  zx_status_t result =
      mexec_identity_aspace->AllocPhysical("1:1 mapping", kBytesToIdentityMap, &identity_address, 0,
                                           pa, VmAspace::VMM_FLAG_VALLOC_SPECIFIC, perm_flags_rwx);
  if (result != ZX_OK) {
    panic("failed to identity map low memory");
  }

  result = alloc_pages_greater_than(final_bootimage_addr + final_bootimage_len + PAGE_SIZE,
                                    kTotalPageTableCount, kBytesToIdentityMap, mexec_safe_pages);
  if (result != ZX_OK) {
    panic("failed to alloc mexec_safe_pages");
  }
}

void platform_mexec(mexec_asm_func mexec_assembly, memmov_ops_t* ops, uintptr_t new_bootimage_addr,
                    size_t new_bootimage_len, uintptr_t new_kernel_entry) {
  DEBUG_ASSERT(arch_ints_disabled());
  DEBUG_ASSERT(mp_get_online_mask() == cpu_num_to_mask(BOOT_CPU_ID));

  // This code only handles one L3 and one L4 page table for now. Fail if
  // there are more L2 page tables than can fit in one L3 page table.
  static_assert(kNumL2PageTables <= NO_OF_PT_ENTRIES,
                "Kexec identity map size is too large. Only one L3 PTE is supported at this time.");
  static_assert(kNumL3PageTables == 1, "Only 1 L3 page table is supported at this time.");
  static_assert(kNumL4PageTables == 1, "Only 1 L4 page table is supported at this time.");
  DEBUG_ASSERT(mexec_identity_aspace);

  vmm_set_active_aspace(mexec_identity_aspace.get());

  size_t safe_page_id = 0;
  volatile pt_entry_t* ptl4 = (pt_entry_t*)paddr_to_physmap(mexec_safe_pages[safe_page_id++]);
  volatile pt_entry_t* ptl3 = (pt_entry_t*)paddr_to_physmap(mexec_safe_pages[safe_page_id++]);

  // Initialize these to 0
  for (size_t i = 0; i < NO_OF_PT_ENTRIES; i++) {
    ptl4[i] = 0;
    ptl3[i] = 0;
  }

  for (size_t i = 0; i < kNumL2PageTables; i++) {
    ptl3[i] = mexec_safe_pages[safe_page_id] | X86_KERNEL_PD_FLAGS;
    volatile pt_entry_t* ptl2 = (pt_entry_t*)paddr_to_physmap(mexec_safe_pages[safe_page_id]);

    for (size_t j = 0; j < NO_OF_PT_ENTRIES; j++) {
      ptl2[j] = (2 * MB * (i * NO_OF_PT_ENTRIES + j)) | X86_KERNEL_PD_LP_FLAGS;
    }

    safe_page_id++;
  }

  ptl4[0] = vaddr_to_paddr((void*)ptl3) | X86_KERNEL_PD_FLAGS;

  mexec_assembly((uintptr_t)new_bootimage_addr, vaddr_to_paddr((void*)ptl4), 0, 0, ops,
                 new_kernel_entry);
}

void platform_early_init(void) {
  /* extract bootloader data while still accessible */
  /* this includes debug uart config, etc. */
  platform_save_bootloader_data();

  /* is the cmdline option to bypass dlog set ? */
  dlog_bypass_init();

#if WITH_LEGACY_PC_CONSOLE
  /* get the text console working */
  platform_init_console();
#endif

  /* initialize physical memory arenas */
  pc_mem_init(gPhysHandoff->memory.get());
}

void platform_prevm_init() {}

// Maps from contiguous id to APICID.
static fbl::Vector<uint32_t> apic_ids;
static size_t bsp_apic_id_index;

static void traverse_topology(uint32_t) {
  // Filter out hyperthreads if we've been told not to init them
  const bool use_ht = gBootOptions->smp_ht_enabled;

  // We're implicitly running on the BSP
  const uint32_t bsp_apic_id = apic_local_id();
  DEBUG_ASSERT(bsp_apic_id == apic_bsp_id());

  // Maps from contiguous id to logical id in topology.
  fbl::Vector<cpu_num_t> logical_ids;

  // Iterate over all the cores, copy apic ids of active cores into list.
  dprintf(INFO, "cpu list:\n");
  size_t cpu_index = 0;
  bsp_apic_id_index = 0;
  for (const auto* processor_node : system_topology::GetSystemTopology().processors()) {
    const auto& processor = processor_node->entity.processor;
    for (size_t i = 0; i < processor.architecture_info.x64.apic_id_count; i++) {
      const uint32_t apic_id = processor.architecture_info.x64.apic_ids[i];
      const bool keep = (i < 1) || use_ht;
      const size_t index = cpu_index++;

      dprintf(INFO, "\t%3zu: apic id %#4x %s%s%s\n", index, apic_id, (i > 0) ? "SMT " : "",
              (apic_id == bsp_apic_id) ? "BSP " : "", keep ? "" : "(not using)");

      if (keep) {
        if (apic_id == bsp_apic_id) {
          bsp_apic_id_index = apic_ids.size();
        }

        fbl::AllocChecker ac;
        apic_ids.push_back(apic_id, &ac);
        if (!ac.check()) {
          dprintf(CRITICAL, "Failed to allocate apic_ids table, disabling SMP!\n");
          return;
        }
        logical_ids.push_back(static_cast<cpu_num_t>(index), &ac);
        if (!ac.check()) {
          dprintf(CRITICAL, "Failed to allocate logical_ids table, disabling SMP!\n");
          return;
        }
      }
    }
  }

  // Find the CPU count limit
  uint32_t max_cpus = gBootOptions->smp_max_cpus;
  if (max_cpus > SMP_MAX_CPUS || max_cpus <= 0) {
    printf("invalid kernel.smp.maxcpus value, defaulting to %d\n", SMP_MAX_CPUS);
    max_cpus = SMP_MAX_CPUS;
  }

  dprintf(INFO, "Found %zu cpu%c\n", apic_ids.size(), (apic_ids.size() > 1) ? 's' : ' ');
  if (apic_ids.size() > max_cpus) {
    dprintf(INFO, "Clamping number of CPUs to %u\n", max_cpus);
    while (apic_ids.size() > max_cpus) {
      apic_ids.pop_back();
      logical_ids.pop_back();
    }
  }

  if (apic_ids.size() == max_cpus || !use_ht) {
    // If we are at the max number of CPUs, or have filtered out
    // hyperthreads, safety check the bootstrap processor is in the set.
    bool found_bp = false;
    for (const auto apic_id : apic_ids) {
      if (apic_id == bsp_apic_id) {
        found_bp = true;
        break;
      }
    }
    ASSERT(found_bp);
  }

  // Construct a distance map from the system topology.
  // The passed lambda is call for every pair of logical processors in the system.
  const size_t cpu_count = logical_ids.size();

  // Record the lowest level at which cpus are shared in the hierarchy, used later to
  // set the global distance threshold.
  unsigned int lowest_sharing_level = 4;  // Start at the highest level we might compute.
  CpuDistanceMap::Initialize(
      cpu_count, [&logical_ids, &lowest_sharing_level](cpu_num_t from_id, cpu_num_t to_id) {
        using system_topology::Node;
        using system_topology::Graph;

        const cpu_num_t logical_from_id = logical_ids[from_id];
        const cpu_num_t logical_to_id = logical_ids[to_id];
        const Graph& topology = system_topology::GetSystemTopology();

        Node* from_node = nullptr;
        if (topology.ProcessorByLogicalId(logical_from_id, &from_node) != ZX_OK) {
          printf("Failed to get processor node for logical CPU %u\n", logical_from_id);
          return -1;
        }
        DEBUG_ASSERT(from_node != nullptr);

        Node* to_node = nullptr;
        if (topology.ProcessorByLogicalId(logical_to_id, &to_node) != ZX_OK) {
          printf("Failed to get processor node for logical CPU %u\n", logical_to_id);
          return -1;
        }
        DEBUG_ASSERT(to_node != nullptr);

        // If the logical cpus are in the same node, they're distance 1
        // TODO: consider SMT as a closer level than cache?
        if (from_node == to_node) {
          return 1;
        }

        // Given a level of topology, return true if the two cpus have a shared parent node.
        auto is_shared_at_level = [&](uint64_t type) -> bool {
          const Node* from_level_node = nullptr;
          for (const Node* node = from_node->parent; node != nullptr; node = node->parent) {
            if (node->entity.discriminant == type) {
              from_level_node = node;
              break;
            }
          }
          const Node* to_level_node = nullptr;
          for (const Node* node = to_node->parent; node != nullptr; node = node->parent) {
            if (node->entity.discriminant == type) {
              to_level_node = node;
              break;
            }
          }

          return (from_level_node && from_level_node == to_level_node);
        };

        // If we've detected the same cache node, then we are level 1
        if (is_shared_at_level(ZBI_TOPOLOGY_ENTITY_CACHE)) {
          lowest_sharing_level = ktl::min(lowest_sharing_level, 1u);
          return 1;
        }

        // If we're on the same die, we're level 2
        if (is_shared_at_level(ZBI_TOPOLOGY_ENTITY_DIE)) {
          lowest_sharing_level = ktl::min(lowest_sharing_level, 2u);
          return 2;
        }

        // If we're on the same socket, we're level 3
        if (is_shared_at_level(ZBI_TOPOLOGY_ENTITY_SOCKET)) {
          lowest_sharing_level = ktl::min(lowest_sharing_level, 3u);
          return 3;
        }

        // Above socket level is all distance 4
        lowest_sharing_level = ktl::min(lowest_sharing_level, 4u);
        return 4;
      });

  // Set the point at which we should consider scheduling to be distant. Set it
  // one past the point a which we started seeing some sharing at the cache, die,
  // or socket level.
  // Limitations: does not handle asymmetric topologies, such as hybrid cpus
  // with dissimilar cpu clusters.
  const CpuDistanceMap::Distance kDistanceThreshold = lowest_sharing_level + 1;
  CpuDistanceMap::Get().set_distance_threshold(kDistanceThreshold);

  CpuDistanceMap::Get().Dump();
}
LK_INIT_HOOK(pc_traverse_topology, traverse_topology, LK_INIT_LEVEL_TOPOLOGY)

// Must be called after traverse_topology has processed the SMP data.
static void platform_init_smp() {
  x86_init_smp(apic_ids.data(), static_cast<uint32_t>(apic_ids.size()));

  // trim the boot cpu out of the apic id list before passing to the AP booting routine
  apic_ids.erase(bsp_apic_id_index);

  x86_bringup_aps(apic_ids.data(), static_cast<uint32_t>(apic_ids.size()));
}

zx_status_t platform_mp_prep_cpu_unplug(cpu_num_t cpu_id) {
  // TODO: Make sure the IOAPIC and PCI have nothing for this CPU
  return arch_mp_prep_cpu_unplug(cpu_id);
}

zx_status_t platform_mp_cpu_unplug(cpu_num_t cpu_id) { return arch_mp_cpu_unplug(cpu_id); }

const char* manufacturer = "unknown";
const char* product = "unknown";

void platform_init(void) {
  platform_init_crashlog();

#if NO_USER_KEYBOARD
  platform_init_keyboard(&console_input_buf);
#endif

  // Initialize all PvEoi instances prior to starting secondary CPUs.
  PvEoi::InitAll();

  platform_init_smp();

  pc_init_smbios();

  SmbiosWalkStructs([](smbios::SpecVersion version, const smbios::Header* h,
                       const smbios::StringTable& st) -> zx_status_t {
    if (h->type == smbios::StructType::SystemInfo && version.IncludesVersion(2, 0)) {
      auto entry = reinterpret_cast<const smbios::SystemInformationStruct2_0*>(h);
      st.GetString(entry->manufacturer_str_idx, &manufacturer);
      st.GetString(entry->product_name_str_idx, &product);
    }
    return ZX_OK;
  });
  printf("smbios: manufacturer=\"%s\" product=\"%s\"\n", manufacturer, product);
}

zx::result<power_cpu_state> platform_get_cpu_state(cpu_num_t cpu_id) {
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}
