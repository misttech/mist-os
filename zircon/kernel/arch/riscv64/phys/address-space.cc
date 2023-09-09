// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "phys/address-space.h"

void ArchSetUpAddressSpaceEarly() {
  // TODO(mcgrathr): unclear if we need identity-mapping or can just leave
  // translation turned off in the satp.
}

void ArchSetUpAddressSpaceLate() {}
