// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_PHYSBOOT_H_
#define ZIRCON_KERNEL_PHYS_PHYSBOOT_H_

class PhysBootTimes;
class KernelStorage;

extern PhysBootTimes gBootTimes;

[[noreturn]] void BootZircon(KernelStorage kernel_storage);

#endif  // ZIRCON_KERNEL_PHYS_PHYSBOOT_H_
