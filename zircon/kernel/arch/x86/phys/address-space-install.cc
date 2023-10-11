// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/x86/system.h>

#include "phys/address-space.h"

#include <ktl/enforce.h>

void AddressSpace::ArchInstall() const {
  // Disable support for global pages ("page global enable"), which
  // otherwise would not be flushed in the operation below.
  //
  // TODO(fxbug.dev/91187): Enable global pages.
  arch::X86Cr4::Read().set_pge(0).Write();

  // Set the new page table root. This will flush the TLB.
  arch::X86Cr3::Write(root_paddr());
}
