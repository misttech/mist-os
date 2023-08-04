// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_MACROS_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_MACROS_H_

#ifdef __clang_version__
// This curbs some annoying initialization bumps by allowing a mix of c99 and non-c99 designators,
// which seems to be the de-facto way of initializing Python structs.
#define DES_MIX _Pragma("GCC diagnostic ignored \"-Wc99-designator\"")
#define IGNORE_EXTRA_SC _Pragma("GCC diagnostic ignored \"-Wextra-semi\"")
#else
#define DES_MIX
#define IGNORE_EXTRA_SC
#endif

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_MACROS_H_
