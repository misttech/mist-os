// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <asm.h>

// This header defines KERNEL_IMAGE_FILE.
#include "kernel-image.h"

// Pad out to the header size that was allocated in the kernel image layout.
// This ensures that the kernel image is aligned correctly in memory.
.org BOOT_HEADER_SIZE

.incbin KERNEL_IMAGE_FILE, BOOT_HEADER_SIZE
