// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "channel.h"

#include "convert.h"
#include "error.h"
#include "mod.h"
#include "src/developer/ffx/lib/fuchsia-controller/cpp/raii/py_wrapper.h"

namespace channel {

int Channel_init(Channel *self, PyObject *args, PyObject *kwds) {
  static const char *kwlist[] = {"handle", nullptr};
  PyObject *handle = nullptr;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "O", const_cast<char **>(kwlist), &handle)) {
    return -1;
  }
  bool is_handle = PyObject_IsInstance(handle, reinterpret_cast<PyObject *>(&handle::HandleType));
  bool is_long = PyLong_Check(handle);
  if (!is_handle && !is_long) {
    PyErr_SetString(PyExc_TypeError,
                    "Expected 'handle' to be either a fuchsia_controller_py.Handle type or an int");
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
    auto f_handle = reinterpret_cast<handle::Handle *>(handle);
    self->super.handle = f_handle->handle;
    // Nullifies the previous handle, preventing it from closing this channel.
    f_handle->handle = 0;
  }
  return 0;
}

PyObject *Channel_write(Channel *self, PyObject *buf) {
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
  Py_ssize_t handles_len = PyList_Size(handles);
  if (handles_len < 0) {
    return nullptr;
  }
  zx_handle_disposition_t c_handles[handles_len];
  for (Py_ssize_t i = 0; i < handles_len; ++i) {
    auto obj = PyList_GetItem(handles, i);
    if (obj == nullptr) {
      return nullptr;
    }
    auto operation_obj = PyTuple_GetItem(obj, 0);
    if (operation_obj == nullptr) {
      return nullptr;
    }
    auto handle_obj = PyTuple_GetItem(obj, 1);
    if (handle_obj == nullptr) {
      return nullptr;
    }
    auto type_obj = PyTuple_GetItem(obj, 2);
    if (type_obj == nullptr) {
      return nullptr;
    }
    auto rights_obj = PyTuple_GetItem(obj, 3);
    if (rights_obj == nullptr) {
      return nullptr;
    }
    auto result_obj = PyTuple_GetItem(obj, 4);
    if (result_obj == nullptr) {
      return nullptr;
    }
    zx_handle_t handle = convert::PyLong_AsU32(handle_obj);
    if (handle == convert::MINUS_ONE_U32) {
      return nullptr;
    }
    zx_rights_t rights = convert::PyLong_AsU32(rights_obj);
    if (rights == convert::MINUS_ONE_U32) {
      return nullptr;
    }
    zx_obj_type_t obj_type = convert::PyLong_AsU32(type_obj);
    if (obj_type == convert::MINUS_ONE_U32) {
      return nullptr;
    }
    zx_status_t result = static_cast<zx_status_t>(convert::PyLong_AsU32(result_obj));
    if (static_cast<uint32_t>(result) == convert::MINUS_ONE_U32) {
      return nullptr;
    }
    zx_handle_op_t operation = convert::PyLong_AsU32(operation_obj);
    if (operation == convert::MINUS_ONE_U32) {
      return nullptr;
    }
    c_handles[i] = {
        .operation = operation,
        .handle = handle,
        .type = obj_type,
        .rights = rights,
        .result = result,
    };
  }
  Py_buffer view;
  if (PyObject_GetBuffer(bytes, &view, PyBUF_CONTIG_RO) < 0) {
    return nullptr;
  }
  auto status = ffx_channel_write_etc(mod::get_module_state()->ctx, self->super.handle,
                                      reinterpret_cast<const char *>(view.buf),
                                      static_cast<uint64_t>(view.len), c_handles, handles_len);
  if (status != ZX_OK) {
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
    PyBuffer_Release(&view);
    return nullptr;
  }
  PyBuffer_Release(&view);
  Py_RETURN_NONE;
}

PyObject *Channel_read(Channel *self, PyObject *Py_UNUSED(arg)) {
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
  auto res = py::Object(PyTuple_New(2));
  if (res == nullptr) {
    return nullptr;
  }
  auto buf = py::Object(PyByteArray_FromStringAndSize(const_cast<const char *>(c_buf),
                                                      static_cast<Py_ssize_t>(actual_bytes_count)));
  if (buf == nullptr) {
    return nullptr;
  }
  auto handles_list = py::Object(PyList_New(static_cast<Py_ssize_t>(actual_handles_count)));
  if (handles_list == nullptr) {
    return nullptr;
  }
  PyTuple_SET_ITEM(res.get(), 0, buf.take());
  for (uint64_t i = 0; i < actual_handles_count; ++i) {
    zx_handle_t handle = handles[i];
    auto handle_obj = PyObject_CallFunction(
        PyObject_Type(reinterpret_cast<PyObject *>(&(self->super))), "I", handle);
    if (handle_obj == nullptr) {
      return nullptr;
    }
    PyList_SET_ITEM(handles_list.get(), static_cast<Py_ssize_t>(i), handle_obj);
  }
  PyTuple_SET_ITEM(res.get(), 1, handles_list.take());
  return res.take();
}

PyObject *Channel_as_int(Channel *self, PyObject *Py_UNUSED(arg)) {
  return PyLong_FromUnsignedLongLong(self->super.handle);
}

// First arg in this function is ChannelType.
PyObject *Channel_create(PyObject *cls, PyObject *Py_UNUSED(arg)) {
  zx_handle_t hdl0;
  zx_handle_t hdl1;
  ffx_channel_create(mod::get_module_state()->ctx, 0, &hdl0, &hdl1);
  py::Object tuple(PyTuple_New(2));
  if (tuple == nullptr) {
    return nullptr;
  }
  py::Object handle_obj0(PyObject_CallFunction(cls, "I", hdl0));
  if (handle_obj0 == nullptr) {
    return nullptr;
  }
  py::Object handle_obj1(PyObject_CallFunction(cls, "I", hdl1));
  if (handle_obj1 == nullptr) {
    return nullptr;
  }
  PyTuple_SET_ITEM(tuple.get(), 0, handle_obj0.take());
  PyTuple_SET_ITEM(tuple.get(), 1, handle_obj1.take());
  return tuple.take();
}

PyObject *Channel_take(Channel *self, PyObject *Py_UNUSED(arg)) {
  auto result = PyLong_FromUnsignedLongLong(self->super.handle);
  self->super.handle = 0;
  return result;
}

PyObject *Channel_close(Channel *self, PyObject *Py_UNUSED(arg)) {
  ffx_close_handle(self->super.handle);
  self->super.handle = 0;
  Py_RETURN_NONE;
}

PyMethodDef Channel_methods[] = {
    {"write", reinterpret_cast<PyCFunction>(Channel_write), METH_O, nullptr},
    {"read", reinterpret_cast<PyCFunction>(Channel_read), METH_NOARGS, nullptr},
    {"as_int", reinterpret_cast<PyCFunction>(Channel_as_int), METH_NOARGS, nullptr},
    {"take", reinterpret_cast<PyCFunction>(Channel_take), METH_NOARGS,
     "Takes the underlying fidl handle, setting it internally to zero (thus invalidating the "
     "underlying channel). This is used for sending a handle through FIDL function calls."},
    {"close", reinterpret_cast<PyCFunction>(Channel_close), METH_NOARGS,
     "Closes the underlying channel. This will invalidate any other copies of this channel."},
    {"create", reinterpret_cast<PyCFunction>(Channel_create), METH_NOARGS | METH_CLASS,
     "classmethod for creating a pair of FIDL channels. These are connected bidirectionally."},
    {nullptr, nullptr, 0, nullptr}};

DES_MIX PyTypeObject ChannelType = {
    PyVarObject_HEAD_INIT(nullptr, 0)

        .tp_name = "fuchsia_controller_py.Channel",
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_doc =
        "Fuchsia controller FIDL channel. This can be read from and written to.\n"
        "\n"
        "Can be constructed from a Handle object, but keep in mind that this will mark\n"
        "the caller's handle invalid, leaving this channel to be the only owner of the underlying\n"
        "handle.",
    .tp_methods = Channel_methods,
    .tp_base = &handle::HandleType,
    .tp_init = reinterpret_cast<initproc>(Channel_init),
};

}  // namespace channel
