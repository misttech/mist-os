// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZBI_PARSER_INCLUDE_LIB_MISTOS_ZBI_PARSER_BOOTFS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZBI_PARSER_INCLUDE_LIB_MISTOS_ZBI_PARSER_BOOTFS_H_

#include <lib/fit/result.h>
#include <lib/mistos/zbitl/vmo.h>
#include <lib/mistos/zx/resource.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <lib/zbi-format/internal/bootfs.h>
#include <lib/zbitl/items/bootfs.h>
#include <zircon/types.h>

#include <ktl/string_view.h>

namespace zbi_parser {

class Bootfs {
 public:
  Bootfs(zx::unowned_vmar vmar, zx::vmo vmo, zx::resource vmex_resource);
  fit::result<zx_status_t, zx::vmo> Open(ktl::string_view root, ktl::string_view filename,
                                         ktl::string_view purpose);

  bool is_valid() { return is_valid_; }

 private:
  using BootfsReader = zbitl::Bootfs<zbitl::MapOwnedVmo>;
  using BootfsView = zbitl::BootfsView<zbitl::MapOwnedVmo>;

  friend void PrintBootfsError(const BootfsReader::Error& error);

  BootfsReader bootfs_reader_;
  zx::resource vmex_resource_;
  bool is_valid_ = false;
};

}  // namespace zbi_parser

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZBI_PARSER_INCLUDE_LIB_MISTOS_ZBI_PARSER_BOOTFS_H_
