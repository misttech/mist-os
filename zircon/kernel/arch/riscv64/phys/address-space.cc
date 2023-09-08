// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "phys/address-space.h"

#include <lib/arch/cache.h>
#include <lib/arch/riscv64/page-table.h>

void ArchSetUpAddressSpaceEarly() {
  AddressSpace aspace;
  aspace.Init();
  aspace.SetUpIdentityMappings();
  aspace.ArchInstall();
}

void ArchSetUpAddressSpaceLate() {}

void AddressSpace::ArchInstall() const {
  arch::RiscvSatp::Modify([root = root_paddr()](auto& satp) {
    satp.set_mode(arch::RiscvSatp::Mode::kSv39).set_root_address(root).set_asid(0);
  });
  arch::InvalidateLocalTlbs();  // Acts as a barrier as well.
}
