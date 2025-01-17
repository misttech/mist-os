// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_FUCHSIA_CONTROLLER_INTERNAL_MOD_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_FUCHSIA_CONTROLLER_INTERNAL_MOD_H_

#include <fuchsia_controller_abi/abi.h>
#include <fuchsia_controller_abi/utils.h>

#include "fuchsia_controller.h"

namespace mod {

constexpr uint64_t ERR_SCRATCH_LEN = 4096;

// Definition of the module-wide state.
using FuchsiaControllerState = struct {
  char ERR_SCRATCH[ERR_SCRATCH_LEN];
  ffx_lib_context_t *ctx;
};

FuchsiaControllerState *get_module_state();
void dump_python_err();

inline int GenericTypeInit(PyTypeObject **type, PyType_Spec *spec) {
  assert(type != nullptr);
  *type = reinterpret_cast<PyTypeObject *>(PyType_FromSpec(spec));
  if (*type == nullptr) {
    return -1;
  }
  if (PyType_Ready(*type) < 0) {
    return -1;
  }
  return 1;
}

}  // namespace mod

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_FUCHSIA_CONTROLLER_INTERNAL_MOD_H_
