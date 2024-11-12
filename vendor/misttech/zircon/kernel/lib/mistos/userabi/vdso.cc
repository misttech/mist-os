// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "lib/userabi/vdso.h"

#include <vm/vm_aspace.h>

#include "sysret-offsets.h"
#include "vdso-code.h"

uintptr_t VDso::base_address(const fbl::RefPtr<VmMapping>& code_mapping) { return 0; }
