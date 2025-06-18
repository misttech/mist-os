// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PLATFORM_PC_INCLUDE_PLATFORM_PC_PIC_H_
#define ZIRCON_KERNEL_PLATFORM_PC_INCLUDE_PLATFORM_PC_PIC_H_

#include <stdint.h>

void pic_map(uint8_t pic1, uint8_t pic2);
void pic_disable();

#endif  // ZIRCON_KERNEL_PLATFORM_PC_INCLUDE_PLATFORM_PC_PIC_H_
