// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "mod.h"

// Defined in fuchsia_controller_py.cc
extern struct PyModuleDef fidl_codec_mod;

namespace mod {

FidlCodecState *get_module_state() {
  auto mod = PyState_FindModule(&fidl_codec_mod);
  return reinterpret_cast<FidlCodecState *>(PyModule_GetState(mod));
}

}  // namespace mod
