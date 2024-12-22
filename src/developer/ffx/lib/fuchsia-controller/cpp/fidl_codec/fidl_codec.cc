// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#define PY_SSIZE_T_CLEAN

#include "decode.h"
#include "encode.h"
#include "ir.h"
#include "mod.h"
#include "src/developer/ffx/lib/fuchsia-controller/cpp/python/py_header.h"
#include "src/developer/ffx/lib/fuchsia-controller/cpp/raii/py_wrapper.h"

extern struct PyModuleDef libfidl_codec;

namespace {

constexpr PyMethodDef SENTINEL = {nullptr, nullptr, 0, nullptr};

PyMethodDef FidlCodecMethods[] = {
    decode::decode_fidl_response_py_def,
    decode::decode_fidl_request_py_def,
    decode::decode_standalone_py_def,
    encode::encode_fidl_message_py_def,
    encode::encode_fidl_object_py_def,
    ir::add_ir_path_py_def,
    ir::add_ir_paths_py_def,
    ir::get_ir_path_py_def,
    ir::get_method_ordinal_py_def,
    SENTINEL,
};

int FidlCodecModule_clear(PyObject *m) {
  auto state = reinterpret_cast<mod::FidlCodecState *>(PyModule_GetState(m));
  state->~FidlCodecState();
  return 0;
}

PyMODINIT_FUNC PyInit_libfidl_codec() {
  auto m = py::Object(PyModule_Create(&libfidl_codec));
  if (m == nullptr) {
    return nullptr;
  }
  auto state = reinterpret_cast<mod::FidlCodecState *>(PyModule_GetState(m.get()));
  new (state) mod::FidlCodecState();
  return m.take();
}

}  // namespace

struct PyModuleDef libfidl_codec = {
    .m_base = PyModuleDef_HEAD_INIT,
    .m_name = "fidl_codec",
    .m_doc = nullptr,
    .m_size = sizeof(mod::FidlCodecState *),
    .m_methods = FidlCodecMethods,
    .m_clear = FidlCodecModule_clear,
};
