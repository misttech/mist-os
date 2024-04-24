// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZBI_PARSER_INCLUDE_LIB_MISTOS_ZBI_PARSER_ZBI_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZBI_PARSER_INCLUDE_LIB_MISTOS_ZBI_PARSER_ZBI_H_

#include <lib/fit/result.h>
#include <lib/mistos/zbi_parser/option.h>
#include <lib/mistos/zx/vmar.h>
#include <lib/mistos/zx/vmo.h>
#include <zircon/types.h>

namespace zbi_parser {

fit::result<zx_status_t, zx::vmo> GetBootfsFromZbi(const zx::vmar& vmar_self,
                                                   const zx::vmo& zbi_vmo, bool discard);

fit::result<zx_status_t, Options> GetOptionsFromZbi(const zx::vmar& vmar_self, const zx::vmo& zbi);

}  // namespace zbi_parser

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZBI_PARSER_INCLUDE_LIB_MISTOS_ZBI_PARSER_ZBI_H_
