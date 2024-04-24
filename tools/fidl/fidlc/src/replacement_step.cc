// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/src/replacement_step.h"

#include "tools/fidl/fidlc/src/versioning_types.h"

namespace fidlc {

namespace {

Version Start(const Element* element) { return element->availability.range().pair().first; }
Version End(const Element* element) { return element->availability.range().pair().second; }

// Helper for checking `removed` and `replaced` arguments. Usage:
//
// 1. Insert() all potentially removed/replaced elements.
// 2. Check() all potential replacement elements.
// 3. Finish().
//
// For `removed`, it fails if there IS a replacement matching the key.
// For `replaced`, it fails if there IS NOT a replacement matching the key.
template <typename Key>
class Checker {
 public:
  explicit Checker(Reporter* reporter) : reporter_(reporter) {}

  void Insert(Key key, const Element* element) {
    switch (element->availability.ending()) {
      case Availability::Ending::kRemoved:
        removed_map_.try_emplace(key, element);
        break;
      case Availability::Ending::kReplaced:
        replaced_map_.try_emplace(key, element);
        break;
      case Availability::Ending::kNone:
      case Availability::Ending::kInherited:
      case Availability::Ending::kSplit:
        break;
    }
  }

  void Check(Key key, const Element* element) {
    if (auto it = removed_map_.find(key); it != removed_map_.end()) {
      const Element* original = it->second;
      auto span = original->attributes->Get("available")->GetArg("removed")->span;
      reporter_->Fail(ErrRemovedWithReplacement, span, original->GetName(), End(original),
                      element->GetNameSource());
    }
    replaced_map_.erase(key);
  }

  void Finish() {
    for (auto& [key, element] : replaced_map_) {
      auto span = element->attributes->Get("available")->GetArg("replaced")->span;
      reporter_->Fail(ErrReplacedWithoutReplacement, span, element->GetName(), End(element));
    }
  }

 private:
  Reporter* reporter_;
  std::map<Key, const Element*> removed_map_;
  std::map<Key, const Element*> replaced_map_;
};

}  // namespace

void ReplacementStep::RunImpl() {
  CheckDecls();
  CheckMembers();
}

void ReplacementStep::CheckDecls() {
  // Compare decls with the same name whose start/end version coincide.
  auto& declarations = library()->declarations.all;
  Checker<std::pair<std::string_view, Version>> checker(reporter());
  for (auto& [name, decl] : declarations)
    checker.Insert({name, End(decl)}, decl);
  for (auto& [name, decl] : declarations)
    checker.Check({name, Start(decl)}, decl);
  checker.Finish();
}

void ReplacementStep::CheckMembers() {
  // Removed/replaced members will have caused the decl to be split, so for each
  // split decl compare the members before and after.
  auto& declarations = library()->declarations.all;
  for (auto it = declarations.begin(); it != declarations.end(); ++it) {
    Decl* before = it->second;
    if (before->availability.ending() != Availability::Ending::kSplit)
      continue;
    auto next = std::next(it);
    ZX_ASSERT(next != declarations.end());
    Decl* after = next->second;
    ZX_ASSERT_MSG(End(before) == Start(after), "multimap should preserve insertion order");
    Checker<std::string_view> checker(reporter());
    before->ForEachMemberFlattened(
        [&](const Element* member) { checker.Insert(member->GetName(), member); });
    after->ForEachMemberFlattened(
        [&](const Element* member) { checker.Check(member->GetName(), member); });
    checker.Finish();
  }
}

}  // namespace fidlc
