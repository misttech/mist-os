// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "object_converter.h"

#include <zircon/types.h>

#include "py_wrapper.h"
#include "src/developer/ffx/lib/fuchsia-controller/abi/convert.h"

namespace converter {

// Helper func. This attempts to lookup an attribute on an object while not setting an error if the
// attribute does not exist. Can still return an error generally, and is indicated by returning
// nullptr. This is just guaranteed not to occur if the attribute doesn't exist.
//
// If no attr was found, return Py_None. If found, it will return a PyObject
// pointer as a new reference.
PyObject* GetAttr(PyObject* target, std::string_view attr) {
  auto name_obj =
      py::Object(PyUnicode_FromStringAndSize(attr.data(), static_cast<Py_ssize_t>(attr.size())));
  // This shouldn't crop up since the IR would have to somehow contain non-unicode strings, which
  // would cause this code to fail far before it ever reaches this point.
  if (name_obj == nullptr) {
    return nullptr;
  }
  auto child_value = PyObject_GetAttr(target, name_obj.get());
  if (child_value == nullptr) {
    // If child_value is NULL, then the error has been set to an AttributeError, so must be
    // cleared. Otherwise an exception will be thrown.
    PyErr_Clear();
    Py_RETURN_NONE;
  }
  return child_value;
}

bool ObjectConverter::HandleNone(const fidl_codec::Type* type) {
  if (obj_ != Py_None) {
    return false;
  }

  if (!type->Nullable()) {
    PyErr_SetString(PyExc_TypeError, "Converting None to non-nullable FIDL value");
  } else {
    result_ = std::make_unique<fidl_codec::NullValue>();
  }
  return true;
}

void ObjectConverter::VisitStringType(const fidl_codec::StringType* type) {
  if (HandleNone(type)) {
    return;
  }
  const char* str = PyUnicode_AsUTF8(obj_);
  if (str) {
    result_ = std::make_unique<fidl_codec::StringValue>(std::string(str));
  }
}

void ObjectConverter::VisitInteger(bool is_signed) {
  int overflow;
  auto repr = PyLong_AsLongLongAndOverflow(obj_, &overflow);
  if (overflow != 0 && repr == -1) {
    return;
  }
  if (repr == -1 && PyErr_Occurred()) {
    return;
  }
  bool negate = is_signed && repr < 0;
  if (negate) {
    repr = -repr;
  }
  result_ = std::make_unique<fidl_codec::IntegerValue>(static_cast<uint64_t>(repr), negate);
}

void ObjectConverter::VisitBoolType(const fidl_codec::BoolType* type) {
  if (!PyBool_Check(obj_)) {
    PyErr_SetString(PyExc_TypeError, "expected bool type");
    return;
  }
  result_ = std::make_unique<fidl_codec::BoolValue>(obj_ == Py_True ? 1 : 0);
}

void ObjectConverter::VisitStructType(const fidl_codec::StructType* type) {
  if (HandleNone(type)) {
    return;
  }
  std::function<PyObject*(const std::string&)> get_item = [this](const std::string& name) mutable {
    return PyObject_GetAttrString(obj_, name.c_str());
  };
  Py_ssize_t idx = 0;
  if (PyList_Check(obj_)) {
    get_item = [this, &idx](const std::string& /*name*/) mutable {
      auto res = PyList_GetItem(obj_, idx++);
      Py_XINCREF(res);
      return res;
    };
  }
  auto res = std::make_unique<fidl_codec::StructValue>(type->struct_definition());
  for (const auto& member : type->struct_definition().members()) {
    if (!member) {
      continue;
    }
    auto child_item = py::Object(get_item(member->name()));
    if (child_item == nullptr) {
      return;
    }
    auto child = ObjectConverter::Convert(child_item.get(), member->type());
    if (!child) {
      return;
    }
    res->AddField(member.get(), std::move(child));
  }
  result_ = std::move(res);
}

void ObjectConverter::VisitTableType(const fidl_codec::TableType* type) {
  auto res = std::make_unique<fidl_codec::TableValue>(type->table_definition());
  for (const auto& member : type->table_definition().members()) {
    if (!member) {
      continue;
    }
    auto child_value = py::Object(GetAttr(obj_, member->name()));
    if (child_value == nullptr) {
      return;
    }
    if (child_value == Py_None) {
      continue;
    }
    auto converted = ObjectConverter::Convert(child_value.get(), member->type());
    if (converted == nullptr) {
      return;
    }
    res->AddMember(member.get(), std::move(converted));
  }
  result_ = std::move(res);
}

void ObjectConverter::VisitUnionType(const fidl_codec::UnionType* type) {
  if (HandleNone(type)) {
    return;
  }

  for (const auto& member : type->union_definition().members()) {
    if (!member) {
      continue;
    }
    auto child_value = py::Object(GetAttr(obj_, member->name()));
    if (child_value == nullptr) {
      return;
    }
    if (child_value == Py_None) {
      continue;
    }
    auto res = ObjectConverter::Convert(child_value.get(), member->type());
    if (res != nullptr) {
      result_ = std::make_unique<fidl_codec::UnionValue>(*member, std::move(res));
    }
    return;
  }

  PyErr_SetString(PyExc_TypeError, "Unkown union variant.");
}

void ObjectConverter::VisitType(const fidl_codec::Type* type) {
  std::stringstream ss;
  ss << "Unknown FIDL type " << type->Name() << ".";
  PyErr_SetString(PyExc_TypeError, ss.str().c_str());
}

void ObjectConverter::VisitFloat() {
  double res = PyFloat_AsDouble(obj_);
  if (res == -1.0 && PyErr_Occurred()) {
    return;
  }
  result_ = std::make_unique<fidl_codec::DoubleValue>(res);
}

void ObjectConverter::VisitList(const fidl_codec::ElementSequenceType* type,
                                std::optional<size_t> count) {
  if (!count && HandleNone(type)) {
    return;
  }

  if (!PyList_Check(obj_)) {
    PyErr_SetString(PyExc_TypeError, "Expected list type");
    return;
  }

  auto size = PyList_Size(obj_);
  if (count && static_cast<uint32_t>(size) != *count) {
    std::stringstream ss;
    ss << "Expected array of size " << *count << ".";
    PyErr_SetString(PyExc_RuntimeError, ss.str().c_str());
    return;
  }

  auto res = std::make_unique<fidl_codec::VectorValue>();
  for (Py_ssize_t i = 0; i < size; ++i) {
    PyObject* item = PyList_GetItem(obj_, i);
    if (!item) {
      return;
    }

    auto converted = ObjectConverter::Convert(item, type->component_type());
    if (converted == nullptr) {
      return;
    }
    res->AddValue(std::move(converted));
  }
  result_ = std::move(res);
}

void ObjectConverter::VisitArrayType(const fidl_codec::ArrayType* type) {
  VisitList(type, type->count());
}

void ObjectConverter::VisitVectorType(const fidl_codec::VectorType* type) {
  VisitList(type, std::nullopt);
}

void ObjectConverter::VisitUint8Type(const fidl_codec::Uint8Type* type) { VisitInteger(false); }

void ObjectConverter::VisitUint16Type(const fidl_codec::Uint16Type* type) { VisitInteger(false); }

void ObjectConverter::VisitUint32Type(const fidl_codec::Uint32Type* type) { VisitInteger(false); }

void ObjectConverter::VisitUint64Type(const fidl_codec::Uint64Type* type) { VisitInteger(false); }

void ObjectConverter::VisitInt8Type(const fidl_codec::Int8Type* type) { VisitInteger(true); }

void ObjectConverter::VisitInt16Type(const fidl_codec::Int16Type* type) { VisitInteger(true); }

void ObjectConverter::VisitInt32Type(const fidl_codec::Int32Type* type) { VisitInteger(true); }

void ObjectConverter::VisitInt64Type(const fidl_codec::Int64Type* type) { VisitInteger(true); }

void ObjectConverter::VisitEnumType(const fidl_codec::EnumType* type) {
  Py_ssize_t size;
  const char* str = PyUnicode_AsUTF8AndSize(obj_, &size);
  if (str == nullptr) {
    return;
  }
  auto name = std::string(str, size);
  for (const auto& member : type->enum_definition().members()) {
    if (name == member.name()) {
      result_ =
          std::make_unique<fidl_codec::IntegerValue>(member.absolute_value(), member.negative());
    }
  }
  std::stringstream ss;
  ss << "Unexpected enum value: " << name;
  PyErr_SetString(PyExc_TypeError, ss.str().c_str());
}

void ObjectConverter::VisitBitsType(const fidl_codec::BitsType* type) {
  int overflow;
  auto repr = PyLong_AsLongLongAndOverflow(obj_, &overflow);
  if (overflow != 0 && repr == -1) {
    PyErr_SetString(PyExc_OverflowError, "Overflow while converting PyLong to 64 bit value");
    return;
  }
  if (repr == -1 && PyErr_Occurred()) {
    return;
  }
  result_ = std::make_unique<fidl_codec::IntegerValue>(repr, false);
}

void ObjectConverter::VisitHandleType(const fidl_codec::HandleType* type) {
  if (HandleNone(type)) {
    return;
  }
  // For the time being just assumes the FIDL handle is going to be the raw handle number and
  // nothing else, so expecting a PyLong.
  auto handle = convert::PyLong_AsU32(obj_);
  if (handle == convert::MINUS_ONE_U32 && PyErr_Occurred()) {
    return;
  }
  // TODO(fxbug.dev/124288): Use the handle disposition properly. This is leaving it
  // mostly blank.
  zx_handle_disposition_t handle_disp = {
      .handle = handle,
  };
  result_ = std::make_unique<fidl_codec::HandleValue>(handle_disp);
}

void ObjectConverter::VisitFloat32Type(const fidl_codec::Float32Type* type) { VisitFloat(); }

void ObjectConverter::VisitFloat64Type(const fidl_codec::Float64Type* type) { VisitFloat(); }

}  // namespace converter
