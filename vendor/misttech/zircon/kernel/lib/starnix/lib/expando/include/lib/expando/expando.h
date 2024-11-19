// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_EXPANDO_INCLUDE_LIB_EXPANDO_EXPANDO_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_EXPANDO_INCLUDE_LIB_EXPANDO_EXPANDO_H_

#include <lib/mistos/util/btree_map.h>
#include <lib/mistos/util/type_hash.h>
#include <lib/starnix_sync/locks.h>

#include <fbl/ref_ptr.h>

namespace expando {

class ExpandoSlot {
 public:
  virtual uint64_t type_id() = 0;
  virtual ~ExpandoSlot() = default;
};

template <typename T>
class ExpandoSlotImpl : public ExpandoSlot {
 public:
  static ktl::unique_ptr<ExpandoSlot> New(fbl::RefPtr<T> value) {
    fbl::AllocChecker ac;
    auto ptr = new (&ac) ExpandoSlotImpl<T>(value);
    ZX_ASSERT(ac.check());
    return ktl::unique_ptr<ExpandoSlot>(ptr);
  }

  ktl::optional<fbl::RefPtr<T>> downcast() { return value_; }

  uint64_t type_id() final { return mtl::typeid_hash<T>(); }

 private:
  explicit ExpandoSlotImpl(fbl::RefPtr<T> value) : value_(ktl::move(value)) {}

  fbl::RefPtr<T> value_;
};

// A lazy collection of values of every type.
//
// An Expando contains a single instance of every type. The values are instantiated lazily
// when accessed. Useful for letting modules add their own state to context objects without
// requiring the context object itself to know about the types in every module.
//
// Typically the type a module uses in the Expando will be private to that module, which lets
// the module know that no other code is accessing its slot on the expando.
class Expando {
 private:
  starnix_sync::Mutex<util::BTreeMap<uint64_t, ktl::unique_ptr<ExpandoSlot>>> properties_;

 public:
  // Get the slot in the expando associated with the given type.
  //
  // The slot is added to the expando lazily but the same instance is returned every time the
  // expando is queried for the same type.
  template <typename T>
  fbl::RefPtr<T> Get() {
    auto properties = properties_.Lock();
    auto type_id = mtl::typeid_hash<T>();

    auto it = properties->find(type_id);
    if (it == properties->end()) {
      fbl::AllocChecker ac;
      auto value = fbl::MakeRefCountedChecked<T>(&ac);
      ZX_ASSERT(ac.check());
      it = properties->try_emplace(type_id, ktl::move(ExpandoSlotImpl<T>::New(value))).first;
    }

    ZX_ASSERT(type_id == it->second->type_id());
    auto* slot_impl = static_cast<ExpandoSlotImpl<T>*>(it->second.get());
    auto ptr = slot_impl->downcast();
    ZX_ASSERT_MSG(ptr.has_value(), "downcast of expando slot was successful");
    return ptr.value();
  }
};

}  // namespace expando

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_EXPANDO_INCLUDE_LIB_EXPANDO_EXPANDO_H_
