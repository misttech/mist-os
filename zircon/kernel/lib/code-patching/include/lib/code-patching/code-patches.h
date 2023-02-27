// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_CODE_PATCHING_INCLUDE_LIB_CODE_PATCHING_CODE_PATCHES_H_
#define ZIRCON_KERNEL_LIB_CODE_PATCHING_INCLUDE_LIB_CODE_PATCHING_CODE_PATCHES_H_

#include <lib/code-patching/code-patching.h>
#include <lib/fit/function.h>

#include <arch/code-patches/case-id.h>
#include <ktl/byte.h>
#include <ktl/initializer_list.h>
#include <ktl/span.h>

// This applies a single patch by modifying the given instruction sequence
// according to the case ID as documented in <arch/code-patches/case-id.h>,
// defined in //zircon/kernel/arch/$cpu/code-patching. The print function
// should be called with any number of strings to be concatenated to describe
// what was done (with no trailing newline). It must return false if the case
// ID is not recognized.
bool ArchPatchCode(code_patching::Patcher& patcher, ktl::span<ktl::byte> insns, CodePatchId case_id,
                   fit::inline_function<void(ktl::initializer_list<ktl::string_view>)> print);

#endif  // ZIRCON_KERNEL_LIB_CODE_PATCHING_INCLUDE_LIB_CODE_PATCHING_CODE_PATCHES_H_
