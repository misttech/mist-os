// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>

#include "fuchsia_controller.h"

#define PY_SSIZE_T_CLEAN
#include <Python.h>

#define PUBLIC __attribute__((visibility("default")))

#ifdef __clang_version__
// This curbs some annoying initialization bumps by allowing a mix of c99 and non-c99 designators,
// which seems to be the de-facto way of initializing Python structs.
#define DES_MIX _Pragma("GCC diagnostic ignored \"-Wc99-designator\"")
#define IGNORE_EXTRA_SC _Pragma("GCC diagnostic ignored \"-Wextra-semi\"")
#else
#define DES_MIX
#define IGNORE_EXTRA_SC
#endif

namespace {

extern PyTypeObject FidlHandleType;
extern struct PyModuleDef fuchsia_controller_py;

constexpr uint64_t ERR_SCRATCH_LEN = 1024;
// Definition of the module-wide state.
using FuchsiaControllerState = struct {
  char ERR_SCRATCH[ERR_SCRATCH_LEN];
  ffx_lib_context_t *ctx;
};

FuchsiaControllerState *get_module_state() {
  auto mod = PyState_FindModule(&fuchsia_controller_py);
  return reinterpret_cast<FuchsiaControllerState *>(PyModule_GetState(mod));
}

void dump_python_err() {
  auto state = get_module_state();
  PyErr_SetString(PyExc_RuntimeError, state->ERR_SCRATCH);
}

IGNORE_EXTRA_SC
using Context = struct {
  PyObject_HEAD;
  ffx_env_context_t *env_context;
};

void Context_dealloc(Context *self) {
  destroy_ffx_env_context(self->env_context);
  Py_TYPE(self)->tp_free(reinterpret_cast<PyObject *>(self));
}

int Context_init(Context *self, PyObject *args, PyObject *kwds) {
  static const char TYPE_ERROR[] = "`config` must be a dictionary of string key/value pairs";
  static const char *kwlist[] = {"config", nullptr};
  PyObject *config = nullptr;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "O", const_cast<char **>(kwlist), &config)) {
    return -1;
  }
  if (!config) {
    return -1;
  }
  if (!PyDict_Check(config)) {
    PyErr_SetString(PyExc_TypeError, TYPE_ERROR);
    return -1;
  }
  Py_ssize_t config_len = PyDict_Size(config);
  if (config_len < 0) {
    return -1;
  }
  std::unique_ptr<ffx_config_t[]> ffx_config;
  if (config_len > 0) {
    ffx_config = std::make_unique<ffx_config_t[]>(config_len);
  }
  PyObject *py_key = nullptr;
  PyObject *py_value = nullptr;
  Py_ssize_t pos = 0;
  // `pos` is not used for iterating in ffx_config because it is an internal
  // iterator for a sparse map, so does not always increment by one.
  for (Py_ssize_t i = 0; PyDict_Next(config, &pos, &py_key, &py_value); ++i) {
    if (!PyUnicode_Check(py_key)) {
      PyErr_SetString(PyExc_TypeError, TYPE_ERROR);
      return -1;
    }
    const char *key = PyUnicode_AsUTF8(py_key);
    if (key == nullptr) {
      return -1;
    }
    if (!PyUnicode_Check(py_value)) {
      PyErr_SetString(PyExc_TypeError, TYPE_ERROR);
      return -1;
    }
    const char *value = PyUnicode_AsUTF8(py_value);
    if (value == nullptr) {
      return -1;
    }
    ffx_config[i] = {
        .key = key,
        .value = value,
    };
  }
  if (create_ffx_env_context(&self->env_context, get_module_state()->ctx, ffx_config.get(),
                             config_len) != ZX_OK) {
    dump_python_err();
    return -1;
  }
  return 0;
}

PyObject *Context_open_daemon_protocol(Context *self, PyObject *protocol) {
  const char *c_protocol = PyUnicode_AsUTF8(protocol);
  if (c_protocol == nullptr) {
    return nullptr;
  }
  zx_handle_t handle;
  if (ffx_open_daemon_protocol(self->env_context, c_protocol, &handle) != ZX_OK) {
    dump_python_err();
    return nullptr;
  }
  return PyObject_CallFunction(reinterpret_cast<PyObject *>(&FidlHandleType), "I", handle);
}

PyObject *Context_open_target_proxy(Context *self, PyObject *Py_UNUSED(unused)) {
  zx_handle_t handle;
  if (ffx_open_target_proxy(self->env_context, &handle) != ZX_OK) {
    dump_python_err();
    return nullptr;
  }
  return PyObject_CallFunction(reinterpret_cast<PyObject *>(&FidlHandleType), "I", handle);
}

PyObject *Context_open_device_proxy(Context *self, PyObject *moniker) {
  const char *c_moniker = PyUnicode_AsUTF8(moniker);
  if (c_moniker == nullptr) {
    return nullptr;
  }
  zx_handle_t handle;
  if (ffx_open_device_proxy(self->env_context, c_moniker, &handle) != ZX_OK) {
    dump_python_err();
    return nullptr;
  }
  return PyObject_CallFunction(reinterpret_cast<PyObject *>(&FidlHandleType), "I", handle);
}

PyMethodDef Context_methods[] = {
    {"open_daemon_protocol", reinterpret_cast<PyCFunction>(Context_open_daemon_protocol), METH_O,
     nullptr},
    {"open_target_proxy", reinterpret_cast<PyCFunction>(Context_open_target_proxy), METH_NOARGS,
     nullptr},
    {"open_device_proxy", reinterpret_cast<PyCFunction>(Context_open_device_proxy), METH_O,
     nullptr},
    {nullptr, nullptr, 0, nullptr}};

DES_MIX PyTypeObject ContextType = {
    PyVarObject_HEAD_INIT(nullptr, 0)

        .tp_name = "fuchsia_controller_py.Context",
    .tp_basicsize = sizeof(Context),
    .tp_itemsize = 0,
    .tp_dealloc = reinterpret_cast<destructor>(Context_dealloc),
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_doc =
        "Fuchsia controller context. This is the necessary object for interacting with a Fuchsia device.",
    .tp_methods = Context_methods,
    .tp_init = reinterpret_cast<initproc>(Context_init),
    .tp_new = PyType_GenericNew,
};

PyObject *open_handle_notifier(PyObject *self, PyObject *Py_UNUSED(arg)) {
  auto descriptor = ffx_open_handle_notifier(get_module_state()->ctx);
  if (descriptor <= 0) {
    dump_python_err();
    return nullptr;
  }
  return PyLong_FromLong(descriptor);
}

PyMethodDef FuchsiaControllerMethods[] = {
    {"open_handle_notifier", open_handle_notifier, METH_NOARGS,
     "Open the handle notification descriptor. Can only be done once within a module."},
    {nullptr, nullptr, 0, nullptr},
};

IGNORE_EXTRA_SC
using FidlHandle = struct {
  PyObject_HEAD;
  zx_handle_t handle;
};

void FidlHandle_dealloc(FidlHandle *self) {
  ffx_close_handle(self->handle);
  Py_TYPE(self)->tp_free(reinterpret_cast<PyObject *>(self));
}

int FidlHandle_init(FidlHandle *self, PyObject *args, PyObject *kwds) {
  static const char *kwlist[] = {"handle", nullptr};
  zx_handle_t handle;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "I", const_cast<char **>(kwlist), &handle)) {
    return -1;
  }
  self->handle = handle;
  return 0;
}

PyObject *FidlHandle_write(FidlHandle *self, PyObject *buf) {
  if (PyTuple_Check(buf) != 1 || PyTuple_Size(buf) != 2) {
    PyErr_SetString(PyExc_TypeError, "Expected tuple of two elements");
    return nullptr;
  }
  auto bytes = PyTuple_GetItem(buf, 0);
  if (bytes == nullptr) {
    return nullptr;
  }
  auto handles = PyTuple_GetItem(buf, 1);
  if (handles == nullptr) {
    return nullptr;
  }
  if (PyList_Check(handles) != 1) {
    PyErr_SetString(PyExc_TypeError, "Second item in tuple should be a list of handles");
    return nullptr;
  }
  if (PyObject_CheckBuffer(bytes) != 1) {
    PyErr_SetString(PyExc_TypeError, "First item in tuple should be a bytearray");
    return nullptr;
  }
  Py_ssize_t handles_len = PyList_Size(handles);
  if (handles_len < 0) {
    return nullptr;
  }
  zx_handle_t c_handles[handles_len];
  for (Py_ssize_t i = 0; i < handles_len; ++i) {
    auto obj = PyList_GetItem(handles, i);
    if (obj == nullptr) {
      return nullptr;
    }
    int res = PyObject_IsInstance(obj, PyObject_Type(reinterpret_cast<PyObject *>(self)));
    if (res < 0) {
      return nullptr;
    }
    if (res != 1) {
      PyErr_SetString(PyExc_TypeError, "All elements in list must be a FidlHandle type.");
      return nullptr;
    }
    auto handle = reinterpret_cast<FidlHandle *>(obj);
    c_handles[i] = handle->handle;
  }
  Py_buffer view;
  if (PyObject_GetBuffer(bytes, &view, PyBUF_CONTIG_RO) < 0) {
    return nullptr;
  }
  if (ffx_channel_write(get_module_state()->ctx, self->handle,
                        reinterpret_cast<const char *>(view.buf), static_cast<uint64_t>(view.len),
                        c_handles, handles_len) != ZX_OK) {
    dump_python_err();
    PyBuffer_Release(&view);
    return nullptr;
  }
  PyBuffer_Release(&view);
  Py_RETURN_NONE;
}

PyObject *FidlHandle_read(FidlHandle *self, PyObject *Py_UNUSED(arg)) {
  // This is the max FIDL message size;
  static constexpr uint64_t c_buf_len = 65536;
  static char c_buf[c_buf_len] = {};
  static constexpr uint64_t handles_len = 64;
  static zx_handle_t handles[handles_len] = {};
  static uint64_t actual_bytes_count = 0;
  static uint64_t actual_handles_count = 0;

  auto status = ffx_channel_read(get_module_state()->ctx, self->handle, c_buf, c_buf_len, handles,
                                 handles_len, &actual_bytes_count, &actual_handles_count);
  if (status != ZX_OK) {
    dump_python_err();
    return nullptr;
  }
  auto res = PyTuple_New(2);
  if (res == nullptr) {
    return nullptr;
  }
  auto buf = PyByteArray_FromStringAndSize(const_cast<const char *>(c_buf),
                                           static_cast<Py_ssize_t>(actual_bytes_count));
  if (buf == nullptr) {
    return nullptr;
  }
  auto handles_list = PyList_New(static_cast<Py_ssize_t>(actual_handles_count));
  if (handles_list == nullptr) {
    return nullptr;
  }
  PyTuple_SET_ITEM(res, 0, buf);
  for (uint64_t i = 0; i < actual_handles_count; ++i) {
    zx_handle_t handle = handles[i];
    auto handle_obj =
        PyObject_CallFunction(PyObject_Type(reinterpret_cast<PyObject *>(self)), "I", handle);
    if (handle_obj == nullptr) {
      return nullptr;
    }
    PyList_SET_ITEM(handles_list, static_cast<Py_ssize_t>(i), handle_obj);
  }
  PyTuple_SET_ITEM(res, 1, handles_list);
  return res;
}

PyMethodDef FidlHandle_methods[] = {
    {"write", reinterpret_cast<PyCFunction>(FidlHandle_write), METH_O, nullptr},
    {"read", reinterpret_cast<PyCFunction>(FidlHandle_read), METH_NOARGS, nullptr},
    {nullptr, nullptr, 0, nullptr}};

DES_MIX PyTypeObject FidlHandleType = {
    PyVarObject_HEAD_INIT(nullptr, 0)

        .tp_name = "fuchsia_controller_py.FidlHandle",
    .tp_basicsize = sizeof(FidlHandle),
    .tp_itemsize = 0,
    .tp_dealloc = reinterpret_cast<destructor>(FidlHandle_dealloc),
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_doc =
        "Fuchsia controller FIDL handle. This is used to bootstrap processes for FIDL interactions.",
    .tp_methods = FidlHandle_methods,
    .tp_init = reinterpret_cast<initproc>(FidlHandle_init),
    .tp_new = PyType_GenericNew,
};

int FuchsiaControllerModule_clear(PyObject *m) {
  auto state = reinterpret_cast<FuchsiaControllerState *>(PyModule_GetState(m));
  destroy_ffx_lib_context(state->ctx);
  return 0;
}

DES_MIX struct PyModuleDef fuchsia_controller_py = {
    PyModuleDef_HEAD_INIT,
    .m_name = "fuchsia_controller_py",
    .m_doc = nullptr,
    .m_size = sizeof(FuchsiaControllerState),
    .m_methods = FuchsiaControllerMethods,
    .m_clear = FuchsiaControllerModule_clear,
};

PyMODINIT_FUNC PUBLIC PyInit_fuchsia_controller_py() {
  if (PyType_Ready(&ContextType) < 0) {
    return nullptr;
  }
  if (PyType_Ready(&FidlHandleType) < 0) {
    return nullptr;
  }
  PyObject *m = PyModule_Create(&fuchsia_controller_py);
  if (m == nullptr) {
    return nullptr;
  }
  auto state = reinterpret_cast<FuchsiaControllerState *>(PyModule_GetState(m));
  create_ffx_lib_context(&state->ctx, state->ERR_SCRATCH, ERR_SCRATCH_LEN);
  Py_INCREF(&ContextType);
  if (PyModule_AddObject(m, "Context", reinterpret_cast<PyObject *>(&ContextType)) < 0) {
    Py_DECREF(&ContextType);
    Py_DECREF(m);
    return nullptr;
  }
  Py_INCREF(&FidlHandleType);
  if (PyModule_AddObject(m, "FidlHandle", reinterpret_cast<PyObject *>(&FidlHandleType)) < 0) {
    Py_DECREF(&FidlHandleType);
    Py_DECREF(m);
    return nullptr;
  }
  return m;
}

}  // namespace
