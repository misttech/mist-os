// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_INSTRUMENTATION_INCLUDE_LIB_INSTRUMENTATION_DEBUGDATA_H_
#define ZIRCON_KERNEL_LIB_INSTRUMENTATION_INCLUDE_LIB_INSTRUMENTATION_DEBUGDATA_H_

#include <zircon/assert.h>
#include <zircon/types.h>

#include <ktl/array.h>
#include <ktl/span.h>
#include <ktl/string_view.h>

namespace instrumentation {

using VmoName = ktl::array<char, ZX_MAX_NAME_LEN>;

// Higher level components will search under this root for debug data files from early boot.
constexpr ktl::string_view kRootPath = "i/";

// Debugdata files from early boot under this directory imply that the data is in flux, and
// continuously being updated.
constexpr ktl::string_view kDynamicSubDir = "/d/";

// Debugdata files from early boot under this directory imply that the data is static, and wont
// change.
constexpr ktl::string_view kStaticSubDir = "/s/";

// Returns an encoded path that fits within an object name. The encoded path is of the form:
//   "$PATH = $ROOT_DIR/$SINK_NAME/$DATA_TYPE/$MODULE_NAME.$SUFFIX"
//
// Given the length limit of an object's name property, and it's truncated to fit.
//
// It is considered if at least the module name does not fit into the encoded path.
inline VmoName DebugdataVmoName(ktl::string_view sink_name, ktl::string_view module_name,
                                ktl::string_view suffix, bool is_static) {
  VmoName name = {};
  ktl::span<char> buffer(name);
  ktl::string_view kDataType = is_static ? kStaticSubDir : kDynamicSubDir;

  // At least some part of the module name to fit in the name.
  ZX_ASSERT(kRootPath.length() + sink_name.length() + kDataType.length() + module_name.length() +
                4 <
            ZX_MAX_NAME_LEN);
  for (ktl::string_view component : {kRootPath, sink_name, kDataType, module_name}) {
    buffer = buffer.subspan(component.copy(buffer.data(), buffer.size() - 1));
    if (buffer.empty()) {
      break;
    }
  }

  // Either append full suffix or nothing at all.
  if (buffer.size() >= suffix.size() + 1) {
    buffer[0] = '.';
    suffix.copy(buffer.subspan(1).data(), buffer.size() - 1);
  }
  return name;
}

}  // namespace instrumentation

#endif  // ZIRCON_KERNEL_LIB_INSTRUMENTATION_INCLUDE_LIB_INSTRUMENTATION_DEBUGDATA_H_
