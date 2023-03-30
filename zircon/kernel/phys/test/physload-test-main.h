// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_TEST_PHYSLOAD_TEST_MAIN_H_
#define ZIRCON_KERNEL_PHYS_TEST_PHYSLOAD_TEST_MAIN_H_

#include <phys/kernel-package.h>

// This function gives the main routine of a phyload module test (i.e., test
// logic packaged as a physload module). Implementations of this function are
// called within a test implementation of PhysLoadModuleMain().
int PhysLoadTestMain(KernelStorage kernel_storage);

#endif  // ZIRCON_KERNEL_PHYS_TEST_PHYSLOAD_TEST_MAIN_H_
