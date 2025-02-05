// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "mod.h"

// Defined in fuchsia_controller_py.cc
extern struct PyModuleDef fuchsia_controller_internal;

namespace mod {

FuchsiaControllerState *get_module_state() {
  auto mod = PyState_FindModule(&fuchsia_controller_internal);
  return reinterpret_cast<FuchsiaControllerState *>(PyModule_GetState(mod));
}

void dump_python_err() {
  auto state = get_module_state();
  PyErr_SetString(PyExc_RuntimeError, state->ERR_SCRATCH);
}

}  // namespace mod
