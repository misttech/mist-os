// Copyright 2025 Mist Tecnologia Ltda
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <zircon/syscalls/pci.h>
#include <zircon/types.h>

zx_status_t zx_pci_add_subtract_io_range(uint32_t mmio, uint64_t base, uint64_t len, uint32_t add);
zx_status_t zx_pci_init(zx_pci_init_arg_t* arg, uint32_t len);
