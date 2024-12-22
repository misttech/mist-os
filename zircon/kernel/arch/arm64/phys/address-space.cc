// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "phys/address-space.h"

#include <lib/arch/arm64/system.h>
#include <lib/arch/cache.h>
#include <lib/arch/paging.h>
#include <lib/boot-options/boot-options.h>
#include <lib/memalloc/pool.h>
#include <lib/memalloc/range.h>
#include <lib/zbi-format/zbi.h>

#include <cstdint>

#include <ktl/byte.h>
#include <ktl/optional.h>
#include <ktl/type_traits.h>
#include <phys/allocation.h>
#include <phys/arch/address-space.h>

#include <ktl/enforce.h>

namespace {

// Set the Intermediate Physical address Size (IPS) or Physical address Size (PS)
// value of the ArmTcrElX register.
//
// This value in the register limits the range of addressable physical memory.
void SetPhysicalAddressSize(arch::ArmTcrEl1& tcr, arch::ArmPhysicalAddressSize size) {
  tcr.set_ips(size);
}
void SetPhysicalAddressSize(arch::ArmTcrEl2& tcr, arch::ArmPhysicalAddressSize size) {
  tcr.set_ps(size);
}

// Set up and enable the MMU with the given page table root.
//
// The template parameters indicate which hardware registers to use,
// and will depend at which EL level we are running at.
template <typename TcrReg, typename SctlrReg, typename Ttbr0Reg, typename Ttbr1Reg,
          typename MairReg>
void EnablePagingForEl(const AddressSpace& aspace) {
  constexpr bool kConfigureUpper = !ktl::is_void_v<Ttbr1Reg>;

  // Ensure caches and MMU disabled.
  arch::ArmSystemControlRegister sctlr_reg = SctlrReg::Read();
  ZX_ASSERT(!sctlr_reg.m() && !sctlr_reg.c());

  // Clear out the data and instruction caches, and all TLBs.
  arch::InvalidateLocalCaches();
  arch::InvalidateLocalTlbs();
  arch::DeviceMemoryBarrier();
  __isb(ARM_MB_SY);

  // Set up the Memory Attribute Indirection Register (MAIR).
  MairReg::Write(aspace.state().mair.reg_value());

  // Configure the page table layout of TTBR0 and enable page table caching.
  TcrReg tcr;
  tcr.set_tg0(arch::ArmTcrTg0Value::k4KiB)                            // Use 4 KiB granules.
      .set_t0sz(64 - AddressSpace::LowerPaging::kVirtualAddressSize)  // Set region size.
      .set_sh0(aspace.state().shareability)
      .set_orgn0(kArchNormalMemoryType.outer)
      .set_irgn0(kArchNormalMemoryType.inner);
  if constexpr (kConfigureUpper) {
    tcr.set_tg1(arch::ArmTcrTg1Value::k4KiB)
        .set_t1sz(64 - AddressSpace::UpperPaging::kVirtualAddressSize)
        .set_sh1(aspace.state().shareability)
        .set_orgn1(kArchNormalMemoryType.outer)
        .set_irgn1(kArchNormalMemoryType.inner);
  }

  // Allow the CPU to access all of its supported physical address space.
  // If the hardware declares it has support for 52bit PA addresses but we're only
  // using 4K page granules, downgrade to 48 bit PA range. Selecting 52 bits in the TCR
  // in this configuration is treated as reserved.
  auto pa_range = arch::ArmIdAa64Mmfr0El1::Read().pa_range();
  if (pa_range == arch::ArmPhysicalAddressSize::k52Bits) {
    pa_range = arch::ArmPhysicalAddressSize::k48Bits;
  }
  SetPhysicalAddressSize(tcr, pa_range);

  // Commit the TCR register.
  tcr.Write();
  __isb(ARM_MB_SY);

  // Set the root of the page table.
  Ttbr0Reg::Write(Ttbr0Reg{}.set_addr(aspace.lower_root_paddr()));
  if constexpr (kConfigureUpper) {
    Ttbr1Reg::Write(Ttbr1Reg{}.set_addr(aspace.upper_root_paddr()));
  }
  __isb(ARM_MB_SY);

  // Enable MMU and caches.
  SctlrReg::Modify([](auto& reg) {
    reg.set_m(true)    // Enable MMU
        .set_c(true)   // Allow data caches
        .set_i(true);  // Enable instruction caches.
  });
  __isb(ARM_MB_SY);
}

}  // namespace

// Set up the MMU, having it use the given page table root.
//
// This will perform the correct operations based on the current exception level
// of the processor.
void AddressSpace::ArchInstall() const {
  // Set up page table for EL1 or EL2, depending on which mode we are running in.
  const auto current_el = arch::ArmDropEl3().el();
  switch (current_el) {
    case 1:
      return EnablePagingForEl<arch::ArmTcrEl1, arch::ArmSctlrEl1, arch::ArmTtbr0El1,
                               arch::ArmTtbr1El1, arch::ArmMairEl1>(*this);
    case 2:
      return EnablePagingForEl<arch::ArmTcrEl2, arch::ArmSctlrEl2, arch::ArmTtbr0El2,
                               /*Ttbr1Reg=*/void, arch::ArmMairEl2>(*this);
    default:
      ZX_PANIC("Unsupported ARM64 exception level: %u", static_cast<uint8_t>(current_el));
  }
}

void ArchSetUpAddressSpace(AddressSpace& aspace) {
  if (gBootOptions && !gBootOptions->arm64_phys_mmu) {
    return;
  }

  // The following two attributes and indices must be kept in sync with those
  // used in the kernel proper
  //
  // LINT.IfChange
  auto mair = arch::ArmMemoryAttrIndirectionRegister::Get()
                  .FromValue(0)
                  .SetAttribute(1, ArchMmioMemoryType())
                  .SetAttribute(2, kArchNormalMemoryType);
  // LINT.ThenChange(/zircon/kernel/arch/arm64/include/arch/arm64/mmu.h)
  aspace.Init(mair);
  aspace.SetUpIdentityMappings();
  aspace.Install();
}

// The MMU will be off when the trampoline runs, so there is nothing to do.
void ArchPrepareAddressSpaceForTrampoline() {}
