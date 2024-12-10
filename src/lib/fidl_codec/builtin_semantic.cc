// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/lib/fidl_codec/builtin_semantic.h"

#include <string>

namespace fidl_codec {
namespace semantic {

std::string builtin_semantic_fuchsia_io =
    "library fuchsia.io {\n"
    "  Node::DeprecatedClone {\n"
    "    request.object = handle : 'cloned';\n"
    "  }\n"
    "  Directory::Open {\n"
    "    request.object = handle / request.path;\n"
    "    input_field: request.path;\n"
    "    result: request.object;\n"
    "  }\n"
    "  File::Seek {\n"
    "    input_field: request.origin;\n"
    "    input_field: request.offset;\n"
    "  }\n"
    "  File::Write {\n"
    "    input_field: request.data.size ' bytes';\n"
    "  }\n"
    "}\n";

}  // namespace semantic
}  // namespace fidl_codec
