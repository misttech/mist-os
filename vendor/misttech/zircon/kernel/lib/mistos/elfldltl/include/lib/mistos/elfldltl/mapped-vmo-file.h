// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ELFLDLTL_INCLUDE_LIB_MISTOS_ELFLDLTL_MAPPED_VMO_FILE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ELFLDLTL_INCLUDE_LIB_MISTOS_ELFLDLTL_MAPPED_VMO_FILE_H_

#include <lib/elfldltl/memory.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <lib/zx/result.h>

#include <ktl/algorithm.h>
#include <ktl/move.h>
#include <ktl/optional.h>

namespace elfldltl {

// elfldltl::MappedVmoFile provides the File and Memory APIs and most other
// features of elfldltl::DirectMemory (see <lib/elfldltl/memory.h>), but on
// a read-only mapping of a VMO's entire contents as the file contents.
//
// The object is default-constructible and move-only.  The Init() function uses
// an unowned VMO handle to set up the mapping but does not need the handle
// thereafter.  The mapping will be removed on the object's destruction.
class MappedVmoFile : public DirectMemory {
 public:
  MappedVmoFile() = default;

  MappedVmoFile(const MappedVmoFile&) = delete;

  MappedVmoFile(MappedVmoFile&& other) noexcept
      : DirectMemory(other.image(), other.base()),
        mapped_size_(other.mapped_size_),
        vmar_(other.vmar_->get()) {
    other.set_image({});
    other.mapped_size_ = 0;
  }

  MappedVmoFile& operator=(MappedVmoFile&& other) noexcept {
    auto old_image = image();
    set_image(other.image());
    other.set_image(old_image);
    set_base(other.base());
    ktl::swap(mapped_size_, other.mapped_size_);
    return *this;
  }

  // This initializes for read-only access to the whole VMO.
  // The Memory API considers the start of the VMO to be address zero.
  zx::result<> Init(zx::unowned_vmo vmo, zx::unowned_vmar vmar = zx::vmar::root_self());

  // This initializes for read-write access to the VMO of the given size.  The
  // base address for the Memory API to assign the beginning of the VMO can be
  // set with the optional third argument.
  zx::result<> InitMutable(zx::unowned_vmo vmo, size_t size, uintptr_t base = 0,
                           zx::unowned_vmar vmar = zx::vmar::root_self());

  ~MappedVmoFile();

 private:
  // Make this private so it can't be used.
  using DirectMemory::set_image;

  size_t mapped_size_ = 0;
  zx::unowned_vmar vmar_;
};

}  // namespace elfldltl

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ELFLDLTL_INCLUDE_LIB_MISTOS_ELFLDLTL_MAPPED_VMO_FILE_H_
