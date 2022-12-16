// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Given a Writer implementing the Writer interface in writer-internal.h, provide an api
// over the writer to allow serializing fxt to the Writer.
//
// Based heavily on libTrace in zircon/system/ulib/trace to allow compatibility,
// but modified to enable passing in an arbitrary buffering system.

#ifndef SRC_PERFORMANCE_LIB_FXT_INCLUDE_LIB_FXT_ARGUMENT_H_
#define SRC_PERFORMANCE_LIB_FXT_INCLUDE_LIB_FXT_ARGUMENT_H_

#include <zircon/types.h>

#include "fields.h"
#include "record_types.h"
#include "string_ref.h"

namespace fxt {

// Represents an FXT Argument, a typed Key Value pair.
//
// See also: https://fuchsia.dev/fuchsia-src/reference/tracing/trace-format#arguments
template <ArgumentType argument_type, RefType name_type, RefType val_type = RefType::kId>
class Argument;

template <RefType name_type>
class Argument<ArgumentType::kNull, name_type> {
 public:
  explicit Argument(StringRef<name_type> name) : name_(name) {}
  WordSize PayloadSize() const {
    return WordSize::FromBytes(sizeof(ArgumentHeader)) + name_.PayloadSize();
  }
  uint64_t Header() const {
    return ArgumentFields::Type::Make(ToUnderlyingType(ArgumentType::kNull)) |
           ArgumentFields::ArgumentSize::Make(PayloadSize().SizeInWords()) |
           ArgumentFields::NameRef::Make(name_.HeaderEntry());
  }

  template <typename Reservation>
  void Write(Reservation& res) const {
    res.WriteWord(Header());
    name_.Write(res);
  }

 private:
  StringRef<name_type> name_;
};
#if __cplusplus >= 201703L
template <RefType name_type>
Argument(StringRef<name_type>) -> Argument<ArgumentType::kNull, name_type>;
#endif

template <RefType name_type>
class Argument<ArgumentType::kBool, name_type> {
 public:
  Argument(StringRef<name_type> name, bool val) : name_(name), val_(val) {}
  WordSize PayloadSize() const {
    return WordSize::FromBytes(sizeof(ArgumentHeader)) + name_.PayloadSize();
  }
  uint64_t Header() const {
    return BoolArgumentFields::Value::Make(val_) |
           ArgumentFields::Type::Make(ToUnderlyingType(ArgumentType::kBool)) |
           ArgumentFields::ArgumentSize::Make(PayloadSize().SizeInWords()) |
           ArgumentFields::NameRef::Make(name_.HeaderEntry());
  }

  template <typename Reservation>
  void Write(Reservation& res) const {
    res.WriteWord(Header());
    name_.Write(res);
  }

 private:
  StringRef<name_type> name_;
  bool val_;
};
#if __cplusplus >= 201703L
template <RefType name_type>
Argument(StringRef<name_type>, bool) -> Argument<ArgumentType::kBool, name_type>;
#endif

template <RefType name_type>
class Argument<ArgumentType::kInt32, name_type> {
 public:
  Argument(StringRef<name_type> name, int32_t val) : name_(name), val_(val) {}
  WordSize PayloadSize() const {
    return WordSize::FromBytes(sizeof(ArgumentHeader)) + name_.PayloadSize();
  }
  uint64_t Header() const {
    return Int32ArgumentFields::Value::Make(val_) |
           ArgumentFields::Type::Make(ToUnderlyingType(ArgumentType::kInt32)) |
           ArgumentFields::ArgumentSize::Make(PayloadSize().SizeInWords()) |
           ArgumentFields::NameRef::Make(name_.HeaderEntry());
  }

  template <typename Reservation>
  void Write(Reservation& res) const {
    res.WriteWord(Header());
    name_.Write(res);
  }

 private:
  StringRef<name_type> name_;
  int32_t val_;
};
#if __cplusplus >= 201703L
template <RefType name_type>
Argument(StringRef<name_type>, int32_t) -> Argument<ArgumentType::kInt32, name_type>;
#endif

template <RefType name_type>
class Argument<ArgumentType::kUint32, name_type> {
 public:
  Argument(StringRef<name_type> name, uint32_t val) : name_(name), val_(val) {}
  WordSize PayloadSize() const {
    return WordSize::FromBytes(sizeof(ArgumentHeader)) + name_.PayloadSize();
  }
  uint64_t Header() const {
    return Uint32ArgumentFields::Value::Make(val_) |
           ArgumentFields::Type::Make(ToUnderlyingType(ArgumentType::kUint32)) |
           ArgumentFields::ArgumentSize::Make(PayloadSize().SizeInWords()) |
           ArgumentFields::NameRef::Make(name_.HeaderEntry());
  }

  template <typename Reservation>
  void Write(Reservation& res) const {
    res.WriteWord(Header());
    name_.Write(res);
  }

 private:
  StringRef<name_type> name_;
  uint32_t val_;
};
#if __cplusplus >= 201703L
template <RefType name_type>
Argument(StringRef<name_type>, uint32_t) -> Argument<ArgumentType::kUint32, name_type>;
#endif

template <RefType name_type>
class Argument<ArgumentType::kInt64, name_type> {
 public:
  Argument(StringRef<name_type> name, int64_t val) : name_(name), val_(val) {}
  WordSize PayloadSize() const {
    return WordSize::FromBytes(sizeof(ArgumentHeader)) + name_.PayloadSize() + WordSize(1);
  }
  uint64_t Header() const {
    return ArgumentFields::Type::Make(ToUnderlyingType(ArgumentType::kInt64)) |
           ArgumentFields::ArgumentSize::Make(PayloadSize().SizeInWords()) |
           ArgumentFields::NameRef::Make(name_.HeaderEntry());
  }

  template <typename Reservation>
  void Write(Reservation& res) const {
    res.WriteWord(Header());
    name_.Write(res);
    res.WriteWord(val_);
  }

 private:
  StringRef<name_type> name_;
  int64_t val_;
};
#if __cplusplus >= 201703L
template <RefType name_type>
Argument(StringRef<name_type>, int64_t) -> Argument<ArgumentType::kInt64, name_type>;
#endif

template <RefType name_type>
class Argument<ArgumentType::kUint64, name_type> {
 public:
  Argument(StringRef<name_type> name, uint64_t val) : name_(name), val_(val) {}
  WordSize PayloadSize() const {
    return WordSize::FromBytes(sizeof(ArgumentHeader)) + name_.PayloadSize() + WordSize(1);
  }
  uint64_t Header() const {
    return ArgumentFields::Type::Make(ToUnderlyingType(ArgumentType::kUint64)) |
           ArgumentFields::ArgumentSize::Make(PayloadSize().SizeInWords()) |
           ArgumentFields::NameRef::Make(name_.HeaderEntry());
  }

  template <typename Reservation>
  void Write(Reservation& res) const {
    res.WriteWord(Header());
    name_.Write(res);
    res.WriteWord(val_);
  }

 private:
  StringRef<name_type> name_;
  uint64_t val_;
};
#if __cplusplus >= 201703L
template <RefType name_type>
Argument(StringRef<name_type>, uint64_t) -> Argument<ArgumentType::kUint64, name_type>;
#endif

template <RefType name_type>
class Argument<ArgumentType::kDouble, name_type> {
 public:
  Argument(StringRef<name_type> name, double val) : name_(name), val_(val) {}
  WordSize PayloadSize() const {
    return WordSize::FromBytes(sizeof(ArgumentHeader)) + name_.PayloadSize() + WordSize(1);
  }
  uint64_t Header() const {
    return ArgumentFields::Type::Make(ToUnderlyingType(ArgumentType::kDouble)) |
           ArgumentFields::ArgumentSize::Make(PayloadSize().SizeInWords()) |
           ArgumentFields::NameRef::Make(name_.HeaderEntry());
  }

  template <typename Reservation>
  void Write(Reservation& res) const {
    res.WriteWord(Header());
    name_.Write(res);
    res.WriteBytes(&val_, 8);
  }

 private:
  StringRef<name_type> name_;
  double val_;
};
#if __cplusplus >= 201703L
template <RefType name_type>
Argument(StringRef<name_type>, double) -> Argument<ArgumentType::kDouble, name_type>;
#endif

template <RefType name_type>
class Argument<ArgumentType::kPointer, name_type> {
 public:
  Argument(StringRef<name_type> name, Pointer val) : name_(name), val_(val) {}
  WordSize PayloadSize() const {
    return WordSize::FromBytes(sizeof(ArgumentHeader)) + name_.PayloadSize() + WordSize(1);
  }
  uint64_t Header() const {
    return ArgumentFields::Type::Make(ToUnderlyingType(ArgumentType::kPointer)) |
           ArgumentFields::ArgumentSize::Make(PayloadSize().SizeInWords()) |
           ArgumentFields::NameRef::Make(name_.HeaderEntry());
  }

  template <typename Reservation>
  void Write(Reservation& res) const {
    res.WriteWord(Header());
    name_.Write(res);
    res.WriteWord(val_.ptr);
  }

 private:
  StringRef<name_type> name_;
  Pointer val_;
};

#if __cplusplus >= 201703L
template <RefType name_type>
Argument(StringRef<name_type>, Pointer) -> Argument<ArgumentType::kPointer, name_type>;
#endif

template <RefType name_type>
class Argument<ArgumentType::kKoid, name_type> {
 public:
  Argument(StringRef<name_type> name, Koid val) : name_(name), val_(val) {}
  WordSize PayloadSize() const {
    return WordSize::FromBytes(sizeof(ArgumentHeader)) + name_.PayloadSize() + WordSize(1);
  }
  uint64_t Header() const {
    return ArgumentFields::Type::Make(ToUnderlyingType(ArgumentType::kKoid)) |
           ArgumentFields::ArgumentSize::Make(PayloadSize().SizeInWords()) |
           ArgumentFields::NameRef::Make(name_.HeaderEntry());
  }

  template <typename Reservation>
  void Write(Reservation& res) const {
    res.WriteWord(Header());
    name_.Write(res);
    res.WriteWord(val_.koid);
  }

 private:
  StringRef<name_type> name_;
  Koid val_;
};
#if __cplusplus >= 201703L
template <RefType name_type>
Argument(StringRef<name_type>, Koid) -> Argument<ArgumentType::kKoid, name_type>;
#endif

template <RefType name_type, RefType val_type>
class Argument<ArgumentType::kString, name_type, val_type> {
 public:
  Argument(StringRef<name_type> name, StringRef<val_type> val) : name_(name), val_(val) {}

  WordSize PayloadSize() const {
    return WordSize::FromBytes(sizeof(ArgumentHeader)) + name_.PayloadSize() + val_.PayloadSize();
  }

  uint64_t Header() const {
    return StringArgumentFields::Index::Make(val_.HeaderEntry()) |
           ArgumentFields::Type::Make(ToUnderlyingType(ArgumentType::kString)) |
           ArgumentFields::ArgumentSize::Make(PayloadSize().SizeInWords()) |
           ArgumentFields::NameRef::Make(name_.HeaderEntry());
  }

  template <typename Reservation>
  void Write(Reservation& res) const {
    res.WriteWord(Header());
    name_.Write(res);
    val_.Write(res);
  }

 private:
  StringRef<name_type> name_;
  StringRef<val_type> val_;
};
#if __cplusplus >= 201703L
Argument(StringRef<RefType::kInline>, StringRef<RefType::kId>)
    ->Argument<ArgumentType::kString, RefType::kInline, RefType::kId>;
Argument(StringRef<RefType::kId>, StringRef<RefType::kId>)
    ->Argument<ArgumentType::kString, RefType::kId, RefType::kId>;
Argument(StringRef<RefType::kInline>, StringRef<RefType::kInline>)
    ->Argument<ArgumentType::kString, RefType::kInline, RefType::kInline>;
Argument(StringRef<RefType::kId>, StringRef<RefType::kInline>)
    ->Argument<ArgumentType::kString, RefType::kId, RefType::kInline>;
#endif

}  // namespace fxt

#endif  // SRC_PERFORMANCE_LIB_FXT_INCLUDE_LIB_FXT_ARGUMENT_H_
