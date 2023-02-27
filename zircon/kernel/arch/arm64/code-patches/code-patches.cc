// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/code-patching/code-patches.h>
#include <zircon/assert.h>

#include <cstdint>
#include <cstdio>

#include <arch/code-patches/case-id.h>
#include <phys/symbolize.h>

// Declared in <lib/code-patching/code-patches.h>.
bool ArchPatchCode(code_patching::Patcher& patcher, ktl::span<ktl::byte> insns, CodePatchId case_id,
                   fit::inline_function<void(ktl::initializer_list<ktl::string_view>)> print) {
  switch (case_id) {
    case CodePatchId::kSelfTest:
      patcher.NopFill(insns);
      print({"'smoke test' trap patched"});
      return true;
  }
  return false;
}
