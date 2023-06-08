// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "mutable-abi.h"

// This just defines the exported ABI symbols, which should go into .bss and be
// initialized at runtime.  These are in their own translation unit because the
// stub ld.so for out-of-process dynamic linking needs only this.

[[gnu::visibility("default")]] ld::abi::Abi<> ld::mutable_abi;

[[gnu::visibility("default")]] ld::abi::Abi<>::RDebug ld::mutable_r_debug;
