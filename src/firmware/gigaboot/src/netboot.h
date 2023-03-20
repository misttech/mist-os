// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_FIRMWARE_GIGABOOT_SRC_NETBOOT_H_
#define SRC_FIRMWARE_GIGABOOT_SRC_NETBOOT_H_

#include <stdint.h>
#include <zircon/compiler.h>

__BEGIN_CDECLS

typedef struct {
  uint8_t* data;
  size_t size;    // max size of buffer
  size_t offset;  // write pointer
} nbfile_t;

int netboot_init(const char* nodename, uint32_t namegen);

const char* netboot_nodename(void);

int netboot_poll(void);

void netboot_close(void);

// Ask for a buffer suitable to put the file `name` in
// Return NULL to indicate `name` is not wanted.
nbfile_t* netboot_get_buffer(const char* name, size_t size);

__END_CDECLS

#endif  // SRC_FIRMWARE_GIGABOOT_SRC_NETBOOT_H_
