// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/zbi-boot.h>

#include <phys/boot-zbi.h>

void BootZbi::ZbiBoot(zircon_kernel_t* kernel, void* arg) { arch::ZbiBoot(kernel, arg); }

void BootZbi::ZbiBootRaw(uintptr_t entry, void* data) { arch::ZbiBootRaw(entry, data); }
