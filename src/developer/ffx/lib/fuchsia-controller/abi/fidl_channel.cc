// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fidl_channel.h"

#include "convert.h"
#include "error.h"
#include "mod.h"

namespace fidl_channel {

int FidlChannel_init(FidlChannel *self, PyObject *args, PyObject *kwds) {
  static const char *kwlist[] = {"handle", nullptr};
  PyObject *handle = nullptr;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "O", const_cast<char **>(kwlist), &handle)) {
    return -1;
  }
  bool is_handle =
      PyObject_IsInstance(handle, reinterpret_cast<PyObject *>(&fidl_handle::FidlHandleType));
  bool is_long = PyLong_Check(handle);
  if (!is_handle && !is_long) {
    PyErr_SetString(
        PyExc_TypeError,
        "Expected 'handle' to be either a fuchsia_controller_py.FidlHandle type or an int");
    return -1;
  }

  // Might be a bit too much checking, but just make sure being a long and a handle are
  // actually mutually exclusive of each other.
  assert(is_long != is_handle);
  if (is_long) {
    zx_handle_t converted_handle = convert::PyLong_AsU32(handle);
    if (converted_handle == convert::MINUS_ONE_U32 && PyErr_Occurred()) {
      return -1;
    }
    self->super.handle = converted_handle;
  }
  if (is_handle) {
    auto f_handle = reinterpret_cast<fidl_handle::FidlHandle *>(handle);
    self->super.handle = f_handle->handle;
    // Nullifies the previous handle, preventing it from closing this channel.
    f_handle->handle = 0;
  }
  return 0;
}

PyObject *FidlChannel_write(FidlChannel *self, PyObject *buf) {
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
    auto handle = reinterpret_cast<FidlChannel *>(obj);
    c_handles[i] = handle->super.handle;
  }
  Py_buffer view;
  if (PyObject_GetBuffer(bytes, &view, PyBUF_CONTIG_RO) < 0) {
    return nullptr;
  }
  if (ffx_channel_write(mod::get_module_state()->ctx, self->super.handle,
                        reinterpret_cast<const char *>(view.buf), static_cast<uint64_t>(view.len),
                        c_handles, handles_len) != ZX_OK) {
    mod::dump_python_err();
    PyBuffer_Release(&view);
    return nullptr;
  }
  PyBuffer_Release(&view);
  Py_RETURN_NONE;
}

PyObject *FidlChannel_read(FidlChannel *self, PyObject *Py_UNUSED(arg)) {
  // This is the max FIDL message size;
  static constexpr uint64_t c_buf_len = 65536;
  static char c_buf[c_buf_len] = {};
  static constexpr uint64_t handles_len = 64;
  static zx_handle_t handles[handles_len] = {};
  static uint64_t actual_bytes_count = 0;
  static uint64_t actual_handles_count = 0;

  auto status = ffx_channel_read(mod::get_module_state()->ctx, self->super.handle, c_buf, c_buf_len,
                                 handles, handles_len, &actual_bytes_count, &actual_handles_count);
  if (status != ZX_OK) {
    static_assert(sizeof(long) >= sizeof(status));  // NOLINT
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
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

PyObject *FidlChannel_as_int(FidlChannel *self, PyObject *Py_UNUSED(arg)) {
  return PyLong_FromUnsignedLongLong(self->super.handle);
}

PyMethodDef FidlChannel_methods[] = {
    {"write", reinterpret_cast<PyCFunction>(FidlChannel_write), METH_O, nullptr},
    {"read", reinterpret_cast<PyCFunction>(FidlChannel_read), METH_NOARGS, nullptr},
    {"as_int", reinterpret_cast<PyCFunction>(FidlChannel_as_int), METH_NOARGS, nullptr},
    {nullptr, nullptr, 0, nullptr}};

DES_MIX PyTypeObject FidlChannelType = {
    PyVarObject_HEAD_INIT(nullptr, 0)

        .tp_name = "fuchsia_controller_py.FidlChannel",
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_doc =
        "Fuchsia controller FIDL channel. This can be read from and written to.\n"
        "\n"
        "Can be constructed from a FidlHandle object, but keep in mind that this will mark\n"
        "the caller's handle invalid, leaving this channel to be the only owner of the underlying\n"
        "handle.",
    .tp_methods = FidlChannel_methods,
    .tp_base = &fidl_handle::FidlHandleType,
    .tp_init = reinterpret_cast<initproc>(FidlChannel_init),
};

}  // namespace fidl_channel
