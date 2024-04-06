// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/fidl/fidlc/src/availability_step.h"

#include <zircon/assert.h>

#include "tools/fidl/fidlc/src/compile_step.h"
#include "tools/fidl/fidlc/src/diagnostics.h"
#include "tools/fidl/fidlc/src/flat_ast.h"
#include "tools/fidl/fidlc/src/reporter.h"
#include "tools/fidl/fidlc/src/versioning_types.h"

namespace fidlc {

void AvailabilityStep::RunImpl() {
  PopulateLexicalParents();
  library()->TraverseElements([&](Element* element) { CompileAvailability(element); });
  ValidateAvailabilities();
}

void AvailabilityStep::PopulateLexicalParents() {
  // First, map members to the Decl they occur in.
  for (const auto& entry : library()->declarations.all) {
    Decl* decl = entry.second;
    decl->ForEachMember(
        [this, decl](const Element* member) { lexical_parents_.emplace(member, decl); });
  }

  // Second, map anonymous layouts to the struct/table/union member or method
  // whose type constructor they occur in. We do this with a helpful function
  // that recursively visits all anonymous types in `type_ctor`.
  std::function<void(Element*, const TypeConstructor*)> link_anonymous =
      [&](Element* member, const TypeConstructor* type_ctor) -> void {
    if (type_ctor->layout.IsSynthetic()) {
      auto anon_layout = type_ctor->layout.raw_synthetic().target.element();
      lexical_parents_.emplace(anon_layout, member);
    }
    for (const auto& param : type_ctor->parameters->items) {
      if (auto param_type_ctor = param->AsTypeCtor()) {
        link_anonymous(member, param_type_ctor);
      }
    }
  };

  for (auto& decl : library()->declarations.structs) {
    for (auto& member : decl->members) {
      link_anonymous(&member, member.type_ctor.get());
    }
  }
  for (auto& decl : library()->declarations.tables) {
    for (auto& member : decl->members) {
      link_anonymous(&member, member.type_ctor.get());
    }
  }
  for (auto& decl : library()->declarations.unions) {
    for (auto& member : decl->members) {
      link_anonymous(&member, member.type_ctor.get());
    }
  }
  for (auto& decl : library()->declarations.overlays) {
    for (auto& member : decl->members) {
      link_anonymous(&member, member.type_ctor.get());
    }
  }
  for (auto& protocol : library()->declarations.protocols) {
    for (auto& method : protocol->methods) {
      if (auto& request = method.maybe_request) {
        link_anonymous(&method, request.get());
      }
      if (auto& response = method.maybe_response) {
        link_anonymous(&method, response.get());
      }
    }
  }
  for (auto& decl : library()->declarations.resources) {
    for (auto& property : decl->properties) {
      link_anonymous(&property, property.type_ctor.get());
    }
  }
}

void AvailabilityStep::CompileAvailability(Element* element) {
  if (element->availability.state() != Availability::State::kUnset) {
    // Already compiled.
    return;
  }

  // Inheritance relies on the parent being compiled first.
  if (auto parent = LexicalParent(element)) {
    CompileAvailability(parent);
  }

  // If this is an anonymous layout, don't attempt to compile the attribute
  // since it can result in misleading errors. Instead, rely on
  // VerifyAttributesStep to report an error about the attribute placement.
  if (!element->IsAnonymousLayout()) {
    if (auto* attribute = element->attributes->Get("available")) {
      CompileAvailabilityFromAttribute(element, attribute);
      return;
    }
  }

  // There is no attribute, so simulate an empty one -- unless this is the
  // library declaration, in which case we default to @available(added=HEAD).
  std::optional<Version> default_added;
  if (element->kind == Element::Kind::kLibrary) {
    ZX_ASSERT(element == library());
    library()->platform = Platform::Unversioned();
    default_added = Version::Head();
  }
  bool valid = element->availability.Init({.added = default_added});
  ZX_ASSERT_MSG(valid, "initializing default availability should succeed");
  if (auto source = AvailabilityToInheritFrom(element)) {
    auto result = element->availability.Inherit(source.value());
    ZX_ASSERT_MSG(result.Ok(), "inheriting into default availability should succeed");
  }
}

void AvailabilityStep::CompileAvailabilityFromAttribute(Element* element, Attribute* attribute) {
  CompileStep::CompileAttributeEarly(compiler(), attribute);

  const bool is_library = element->kind == Element::Kind::kLibrary;
  ZX_ASSERT(is_library == (element == library()));

  const auto platform = attribute->GetArg("platform");
  const auto added = attribute->GetArg("added");
  const auto deprecated = attribute->GetArg("deprecated");
  const auto removed = attribute->GetArg("removed");
  const auto replaced = attribute->GetArg("replaced");
  const auto note = attribute->GetArg("note");
  const auto legacy = attribute->GetArg("legacy");

  // These errors do not block further analysis.
  if (!is_library && attribute->args.empty()) {
    reporter()->Fail(ErrAvailableMissingArguments, attribute->span);
  }
  if (note && !deprecated) {
    reporter()->Fail(ErrNoteWithoutDeprecation, attribute->span);
  }

  // These errors block further analysis because we don't know what's intended,
  // and proceeding further will lead to confusing error messages.
  // We use & to report as many errors as possible (&& would short circuit).
  bool ok = true;
  if (is_library) {
    if (!added) {
      ok &= reporter()->Fail(ErrLibraryAvailabilityMissingAdded, attribute->span);
    }
    if (replaced) {
      ok &= reporter()->Fail(ErrLibraryReplaced, replaced->span);
    }
  } else {
    if (platform) {
      ok &= reporter()->Fail(ErrPlatformNotOnLibrary, platform->span);
    }
    if (!library()->attributes->Get("available")) {
      ok &= reporter()->Fail(ErrMissingLibraryAvailability, attribute->span, library()->name);
    }
  }
  if (removed && replaced) {
    ok &= reporter()->Fail(ErrRemovedAndReplaced, attribute->span);
  }
  if (!ok) {
    element->availability.Fail();
    return;
  }

  const auto removed_or_replaced = removed ? removed : replaced;
  const auto init_args = Availability::InitArgs{
      .added = GetVersion(added),
      .deprecated = GetVersion(deprecated),
      .removed = GetVersion(removed_or_replaced),
      .legacy = GetLegacy(legacy),
  };
  if (is_library) {
    const auto library_platform = GetPlatform(platform).value_or(GetDefaultPlatform());
    library()->platform = library_platform;
    if (library_platform.is_unversioned()) {
      reporter()->Fail(ErrReservedPlatform, attribute->span, library_platform);
    } else if (!version_selection()->Contains(library_platform)) {
      reporter()->Fail(ErrPlatformVersionNotSelected, attribute->span, library()->name,
                       library_platform);
    }
    if (!init_args.added) {
      // Return early to avoid letting the -inf from Availability::Unbounded()
      // propagate any further, since .Inherit() asserts added != -inf.
      element->availability.Fail();
      return;
    }
  }
  if (!element->availability.Init(init_args)) {
    std::string msg;
    if (added) {
      msg.append("added");
    }
    if (deprecated) {
      msg.append(msg.empty() ? "deprecated" : " <= deprecated");
    }
    if (removed) {
      msg.append(" < removed");
    } else if (replaced) {
      msg.append(" < replaced");
    }
    reporter()->Fail(ErrInvalidAvailabilityOrder, attribute->span, msg);
    // Return early to avoid confusing error messages about inheritance
    // conflicts for an availability that isn't even self-consistent.
    return;
  }

  // Reports an error for arg given its inheritance status.
  auto report = [&](const AttributeArg* arg, Availability::InheritResult::Status status) {
    const char* when;
    const AttributeArg* inherited_arg;
    switch (status) {
      case Availability::InheritResult::Status::kOk:
        return;
      case Availability::InheritResult::Status::kBeforeParentAdded:
        when = "before";
        inherited_arg = AncestorArgument(element, {"added"});
        break;
      case Availability::InheritResult::Status::kAfterParentDeprecated:
        when = "after";
        inherited_arg = AncestorArgument(element, {"deprecated"});
        break;
      case Availability::InheritResult::Status::kAfterParentRemoved:
        when = "after";
        inherited_arg = AncestorArgument(element, {"removed", "replaced"});
        break;
    }
    auto child_what = arg->name.value().data();
    auto parent_what = inherited_arg->name.value().data();
    reporter()->Fail(ErrAvailabilityConflictsWithParent, arg->span, arg, arg->value->span.data(),
                     inherited_arg, inherited_arg->value->span.data(), inherited_arg->span,
                     child_what, when, parent_what);
  };

  // Reports an error for the legacy arg given its status.
  auto report_legacy = [&](const AttributeArg* arg,
                           Availability::InheritResult::LegacyStatus status) {
    switch (status) {
      case Availability::InheritResult::LegacyStatus::kOk:
        break;
      case Availability::InheritResult::LegacyStatus::kNeverRemoved:
        reporter()->Fail(ErrLegacyWithoutRemoval, arg->span, arg);
        break;
      case Availability::InheritResult::LegacyStatus::kWithoutParent: {
        const auto* inherited_arg = AncestorArgument(element, {"removed", "replaced"});
        reporter()->Fail(ErrLegacyConflictsWithParent, arg->span, arg, arg->value->span.data(),
                         inherited_arg, inherited_arg->value->span.data(), inherited_arg->span);
        break;
      }
    }
  };

  if (auto source = AvailabilityToInheritFrom(element)) {
    const auto result = element->availability.Inherit(source.value());
    report(added, result.added);
    report(deprecated, result.deprecated);
    report(removed_or_replaced, result.removed);
    report_legacy(legacy, result.legacy);
  }
}

Platform AvailabilityStep::GetDefaultPlatform() {
  auto platform = Platform::Parse(std::string(library()->name.front()));
  ZX_ASSERT_MSG(platform, "library component should be valid platform");
  return platform.value();
}

std::optional<Platform> AvailabilityStep::GetPlatform(const AttributeArg* maybe_arg) {
  if (!(maybe_arg && maybe_arg->value->IsResolved())) {
    return std::nullopt;
  }
  ZX_ASSERT(maybe_arg->value->Value().kind == ConstantValue::Kind::kString);
  std::string str =
      static_cast<const StringConstantValue*>(&maybe_arg->value->Value())->MakeContents();
  auto platform = Platform::Parse(str);
  if (!platform) {
    reporter()->Fail(ErrInvalidPlatform, maybe_arg->value->span, str);
    return std::nullopt;
  }
  return platform;
}

std::optional<Version> AvailabilityStep::GetVersion(const AttributeArg* maybe_arg) {
  if (!(maybe_arg && maybe_arg->value->IsResolved())) {
    return std::nullopt;
  }
  // Note: We only have to deal with integers here. If the argument was the
  // identifier `HEAD`, it will have been resolved to Version::Head().ordinal()
  // when we call CompileAttributeEarly. See AttributeArgSchema::ResolveArg.
  ZX_ASSERT(maybe_arg->value->Value().kind == ConstantValue::Kind::kUint64);
  uint64_t value =
      static_cast<const NumericConstantValue<uint64_t>*>(&maybe_arg->value->Value())->value;
  auto version = Version::From(value);
  // Do not allow referencing the LEGACY version directly. It may only be
  // specified on the command line, or in FIDL libraries via the `legacy`
  // argument to @available.
  if (!version || version == Version::Legacy()) {
    reporter()->Fail(ErrInvalidVersion, maybe_arg->value->span, value);
    return std::nullopt;
  }
  return version;
}

std::optional<Availability::Legacy> AvailabilityStep::GetLegacy(const AttributeArg* maybe_arg) {
  if (!(maybe_arg && maybe_arg->value->IsResolved())) {
    return std::nullopt;
  }
  ZX_ASSERT(maybe_arg->value->Value().kind == ConstantValue::Kind::kBool);
  return static_cast<const BoolConstantValue*>(&maybe_arg->value->Value())->value
             ? Availability::Legacy::kYes
             : Availability::Legacy::kNo;
}

std::optional<Availability> AvailabilityStep::AvailabilityToInheritFrom(const Element* element) {
  const Element* parent = LexicalParent(element);
  if (!parent) {
    ZX_ASSERT_MSG(element == library(), "if it has no parent, it must be the library");
    return Availability::Unbounded();
  }
  if (parent->availability.state() == Availability::State::kInherited) {
    // The typical case: inherit from the parent.
    return parent->availability;
  }
  // The parent failed to compile, so don't try to inherit.
  return std::nullopt;
}

const AttributeArg* AvailabilityStep::AncestorArgument(
    const Element* element, const std::vector<std::string_view>& arg_names) {
  while ((element = LexicalParent(element))) {
    if (auto attribute = element->attributes->Get("available")) {
      for (auto name : arg_names) {
        if (auto arg = attribute->GetArg(name)) {
          return arg;
        }
      }
    }
  }
  ZX_PANIC("no ancestor exists for this arg");
}

Element* AvailabilityStep::LexicalParent(const Element* element) {
  ZX_ASSERT(element);
  if (element == library()) {
    return nullptr;
  }
  if (auto it = lexical_parents_.find(element); it != lexical_parents_.end()) {
    return it->second;
  }
  // If it's not in lexical_parents_, it must be a top-level declaration.
  return library();
}

namespace {

struct CmpAvailability {
  bool operator()(const Element* lhs, const Element* rhs) const {
    return lhs->availability.set() < rhs->availability.set();
  }
};

// Helper class that validates:
// * No canonical name collisions on elements with overlapping availabilities
// * For each @available(replaced=N) there IS a corresponding @available(added=N)
// * For each @available(removed=N) there IS NOT a corresponding @available(added=N)
class Validator {
 public:
  Validator(Reporter* reporter, const Platform& platform)
      : reporter_(reporter), platform_(platform) {}
  Validator(const Validator&) = delete;

  void Insert(const Element* element) {
    // Skip elements whose availabilities we failed to compile.
    if (element->availability.state() != Availability::State::kInherited) {
      return;
    }
    auto set = element->availability.set();
    auto added = set.ranges().first.pair().first;
    auto name = element->GetName();
    auto canonical_name = canonicalize(name);
    auto& same_canonical_name = by_canonical_name_[canonical_name];
    CheckForNameCollisions(element, set, name, canonical_name, same_canonical_name);
    same_canonical_name.insert(element);
    by_added_by_name_[name].emplace(added, element);
  }

  ~Validator() {
    for (auto& [name, by_added] : by_added_by_name_) {
      CheckRemovedAndReplacedArguments(name, by_added);
    }
  }

 private:
  // Note: This algorithm is worst-case O(n^2) in the number of elements
  // having the same name. It could be optimized to O(n*log(n)).
  void CheckForNameCollisions(
      const Element* element, const VersionSet& set, std::string_view name,
      std::string_view canonical_name,
      const std::set<const Element*, CmpAvailability>& same_canonical_name) {
    for (auto other : same_canonical_name) {
      auto other_set = other->availability.set();
      auto overlap = VersionSet::Intersect(set, other_set);
      if (!overlap) {
        continue;
      }
      auto span = element->GetNameSource();
      auto other_name = other->GetName();
      auto other_span = other->GetNameSource();
      // Use a simplified error message when availabilities are the identical.
      if (set == other_set) {
        if (name == other_name) {
          reporter_->Fail(ErrNameCollision, span, element->kind, name, other->kind, other_span);
        } else {
          reporter_->Fail(ErrNameCollisionCanonical, span, element->kind, name, other->kind,
                          other_name, other_span, canonical_name);
        }
      } else {
        if (name == other_name) {
          reporter_->Fail(ErrNameOverlap, span, element->kind, name, other->kind, other_span,
                          overlap.value(), platform_);
        } else {
          reporter_->Fail(ErrNameOverlapCanonical, span, element->kind, name, other->kind,
                          other_name, other_span, canonical_name, overlap.value(), platform_);
        }
      }
      // Report at most one error per element to avoid noisy redundant errors.
      break;
    }
  }

  void CheckRemovedAndReplacedArguments(std::string_view name,
                                        const std::map<Version, const Element*>& by_added) {
    for (auto& [_added, element] : by_added) {
      if (auto attribute = element->attributes->Get("available")) {
        auto removed_arg = attribute->GetArg("removed");
        auto replaced_arg = attribute->GetArg("replaced");
        if (!removed_arg && !replaced_arg) {
          continue;
        }
        auto version = element->availability.set().ranges().first.pair().second;
        auto it = by_added.find(version);
        auto replacement = it != by_added.end() ? it->second : nullptr;
        if (removed_arg && replacement) {
          reporter_->Fail(ErrRemovedWithReplacement, removed_arg->span, name, version,
                          replacement->GetNameSource());
        } else if (replaced_arg && !replacement) {
          reporter_->Fail(ErrReplacedWithoutReplacement, replaced_arg->span, name, version);
        }
      }
    }
  }

  Reporter* reporter_;
  const Platform& platform_;
  std::map<std::string, std::set<const Element*, CmpAvailability>> by_canonical_name_;
  std::map<std::string_view, std::map<Version, const Element*>> by_added_by_name_;
};

}  // namespace

void AvailabilityStep::ValidateAvailabilities() {
  auto& platform = library()->platform;
  if (!platform.has_value()) {
    // We failed to compile the library declaration's @available attribute.
    return;
  }
  Validator decl_validator(reporter(), *platform);
  for (auto& [name, decl] : library()->declarations.all) {
    decl_validator.Insert(decl);
    Validator member_validator(reporter(), *platform);
    decl->ForEachMember([&](const Element* member) { member_validator.Insert(member); });
  }
}

}  // namespace fidlc
