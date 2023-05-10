// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Small utility functions.

#ifndef SRC_FIRMWARE_GIGABOOT_SRC_UTIL_H_
#define SRC_FIRMWARE_GIGABOOT_SRC_UTIL_H_

#include <zircon/compiler.h>

__BEGIN_CDECLS

char key_prompt(const char* valid_keys, int timeout_s);

__END_CDECLS

#endif  // SRC_FIRMWARE_GIGABOOT_SRC_UTIL_H_
