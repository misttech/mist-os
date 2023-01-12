// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_FIDL_FIDLC_INCLUDE_FIDL_SOURCE_MAP_H_
#define TOOLS_FIDL_FIDLC_INCLUDE_FIDL_SOURCE_MAP_H_

#include <lib/fit/function.h>
#include <zircon/assert.h>

#include <cstdint>
#include <map>

#include "tools/fidl/fidlc/include/fidl/flat/sourced.h"
#include "tools/fidl/fidlc/include/fidl/flat_ast.h"
#include "tools/fidl/fidlc/include/fidl/raw_ast.h"

namespace fidl {

class SourceMapEntry {
 public:
  enum struct Kind {
    kUnique,
    kVersioned,
  };

  explicit SourceMapEntry(const Kind kind) : kind_(kind) {}
  virtual ~SourceMapEntry() = default;

 private:
  [[maybe_unused]] const Kind kind_;
};

// Certain |flat::*| types must always be in |UniqueEntry<T>|, due to the fact that they do not
// carry availability.
template <typename T>
static constexpr void UniqueRequirement() {
  static_assert(
      std::is_base_of_v<flat::Constant, T> || std::is_base_of_v<flat::LayoutParameter, T> ||
          std::is_same_v<T, flat::Attribute> || std::is_same_v<T, flat::AttributeArg> ||
          std::is_same_v<T, flat::LayoutParameterList> ||
          std::is_same_v<T, flat::LiteralConstant> || std::is_same_v<T, flat::TypeConstraints> ||
          std::is_same_v<T, flat::TypeConstructor>,
      "Not a valid type parameter for UniqueEntry");
}

// A raw AST node that always has exactly one flat AST node to match with. Versioning this node is
// not possible.
template <typename T>
class UniqueEntry final : public SourceMapEntry {
 public:
  explicit UniqueEntry(const T* value)
      : SourceMapEntry(SourceMapEntry::Kind::kUnique), value_(value) {
    UniqueRequirement<T>();
  }

  const T* Get() const { return value_; }

 private:
  const T* value_;
};

// Certain |flat::*| types must always be in |VersionedEntry<T>|, due to the fact that they carry
// availability.
template <typename T>
static constexpr void VersionedRequirement() {
  static_assert(std::is_base_of_v<flat::Element, T>,
                "Not a valid type parameter for VersionedEntry");
}

// Unlike |UniqueEntry|, the flat AST nodes stored in |VersionedEntry<T>| are all |flat::Element|s,
// meaning that a single raw AST node my be broken up into many flat AST nodes, depending on how the
// members are versioned. To account for this, instead of storing a 1:1 raw AST node:flat AST node
// mapping like we do for |UniqueEntry|, we store an ordered map of all of the versions of the
// definition derived from the declaring raw AST node.
template <typename T>
class VersionedEntry final : public SourceMapEntry {
 public:
  explicit VersionedEntry(const T* value) : SourceMapEntry(SourceMapEntry::Kind::kVersioned) {
    VersionedRequirement<T>();
    Add(value);
  }

  void Add(const T* value) {
    auto element = static_cast<const flat::Element*>(value);
    versions_.insert({element->availability.range(), value});
  }

  const T* At(const Version version) const {
    for (const auto& entry : versions_) {
      if (entry.first.Contains(version)) {
        return entry.second;
      }
    }
    return nullptr;
  }

  void ForEach(fit::function<void(const VersionRange&, const T&)> f) const {
    for (const auto& entry : versions_) {
      f(entry.first, *entry.second);
    }
  }

  const T* Oldest() const {
    if (versions_.empty()) {
      return nullptr;
    }
    return versions_.begin()->second;
  }

  const T* Newest() const {
    if (versions_.empty()) {
      return nullptr;
    }
    return std::prev(versions_.end())->second;
  }

  size_t size() const { return versions_.size(); }

  bool empty() const { return versions_.size() == 0; }

 private:
  std::map<VersionRange, const T*> versions_;
};

class SourceMapBuilder;

class SourceMap {
 public:
  const SourceMapEntry* GetEntry(raw::SourceElement::Signature signature) const {
    auto result = map_.find(signature);
    if (result == map_.end()) {
      return nullptr;
    }
    return result->second.get();
  }

  template <typename T>
  const VersionedEntry<T>* GetVersioned(raw::SourceElement::Signature signature) const {
    return static_cast<const VersionedEntry<T>*>(GetEntry(signature));
  }

  template <typename T>
  const UniqueEntry<T>* GetUnique(raw::SourceElement::Signature signature) const {
    return static_cast<const UniqueEntry<T>*>(GetEntry(signature));
  }

  size_t size() const { return map_.size(); }

  bool empty() const { return map_.size() == 0; }

 private:
  friend SourceMapBuilder;
  explicit SourceMap(
      std::unordered_map<raw::SourceElement::Signature, std::unique_ptr<SourceMapEntry>> map)
      : map_(std::move(map)) {}

  std::unordered_map<raw::SourceElement::Signature, std::unique_ptr<SourceMapEntry>> map_;
};

class SourceMapBuilder {
 public:
  SourceMapBuilder() = default;

  SourceMap Build() && { return SourceMap(std::move(map_)); }

  template <typename T>
  void AddVersioned(const T* value) {
    VersionedRequirement<T>();
    static_assert(std::is_base_of_v<flat::Sourced, T> || std::is_base_of_v<flat::MaybeSourced, T>,
                  "Can only add Sourced or MaybeSourced inheriting classes as entries");

    auto add = [&](raw::SourceElement::Signature signature) {
      auto found = map_.find(signature);
      if (found == map_.end()) {
        map_.insert({signature, std::make_unique<VersionedEntry<T>>(value)});
      } else {
        static_cast<VersionedEntry<T>*>(found->second.get())->Add(value);
      }
    };

    if constexpr (std::is_base_of_v<flat::Sourced, T>) {
      auto sourced = static_cast<const flat::Sourced*>(value);
      add(sourced->source_signature());
    } else {
      auto maybe_sourced = static_cast<const flat::MaybeSourced*>(value);
      ZX_ASSERT(maybe_sourced->maybe_source_signature().has_value());
      add(maybe_sourced->maybe_source_signature().value());
    }
  }

  template <typename T>
  void AddUnique(const T* value) {
    UniqueRequirement<T>();
    static_assert(std::is_base_of_v<flat::Sourced, T> || std::is_base_of_v<flat::MaybeSourced, T>,
                  "Can only add Sourced or MaybeSourced inheriting classes as entries");

    if constexpr (std::is_base_of_v<flat::Sourced, T>) {
      auto sourced = static_cast<const flat::Sourced*>(value);
      map_.insert({sourced->source_signature(), std::make_unique<UniqueEntry<T>>(value)});
    } else {
      auto maybe_sourced = static_cast<const flat::MaybeSourced*>(value);
      ZX_ASSERT(maybe_sourced->maybe_source_signature().has_value());
      map_.insert({maybe_sourced->maybe_source_signature().value(),
                   std::make_unique<UniqueEntry<T>>(value)});
    }
  }

 private:
  std::unordered_map<raw::SourceElement::Signature, std::unique_ptr<SourceMapEntry>> map_;
};

}  // namespace fidl

#endif  // TOOLS_FIDL_FIDLC_INCLUDE_FIDL_SOURCE_MAP_H_
