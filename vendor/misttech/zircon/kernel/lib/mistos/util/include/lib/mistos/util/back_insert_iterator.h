// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BACK_INSERTER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BACK_INSERTER_H_

#include <zircon/assert.h>

#include <fbl/alloc_checker.h>
#include <ktl/move.h>

namespace util {

template <typename Container>
class back_insert_iterator {
 public:
  // Standard C++ named requirements Container API.
  using iterator_category = std::output_iterator_tag;
  using value_type = void;
  using difference_type = ptrdiff_t;
  using pointer = void;
  using reference = void;

  explicit back_insert_iterator(Container& container) : container_(&container) {}

  back_insert_iterator& operator=(const typename Container::value_type& value) {
    fbl::AllocChecker ac;
    container_->push_back(value, &ac);
    ZX_ASSERT(ac.check());
    return *this;
  }

  back_insert_iterator& operator=(typename Container::value_type&& value) {
    fbl::AllocChecker ac;
    container_->push_back(ktl::move(value), &ac);
    ZX_ASSERT(ac.check());
    return *this;
  }

  back_insert_iterator& operator*() { return *this; }
  back_insert_iterator& operator++() { return *this; }
  back_insert_iterator& operator++(int) { return *this; }

 private:
  Container* container_;
};

template <class Container>
inline back_insert_iterator<Container> back_inserter(Container& container) {
  return back_insert_iterator<Container>(container);
}

}  // namespace util

#endif  // ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BACK_INSERTER_H_
