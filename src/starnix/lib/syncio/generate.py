#!/usr/bin/env -S python3 -B
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import os, sys
sys.path.append(os.path.dirname(os.path.abspath(__file__)) + '/..')
from bindgen import Bindgen

bindgen = Bindgen()

bindgen.raw_lines = """
use zerocopy::{AsBytes, FromBytes, FromZeroes};
"""

bindgen.include_dirs = [
    'sdk/lib/zxio/include',
    'zircon/third_party/ulib/musl/include',
    'zircon/system/public',
]

bindgen.function_allowlist = ['zxio_.*']
bindgen.var_allowlist = [
    'ZXIO_SHUTDOWN.*', 'ZXIO_NODE_PROTOCOL.*', 'ZXIO_SEEK_ORIGIN.*', 'E[A-Z]*',
    'AF_.*', 'SO.*', 'IP.*', 'MSG_.*'
]
bindgen.type_allowlist = ['cmsghdr.*', 'in6_.*', 'sockaddr.*']

bindgen.set_auto_derive_traits(
    [
        (r'cmsghdr', ['AsBytes, FromBytes', 'FromZeroes']),
        (r'in6_pktinfo', ['AsBytes, FromBytes', 'FromZeroes']),
        (r'in6_addr*', ['AsBytes, FromBytes', 'FromZeroes']),
    ])

bindgen.run(
    'src/starnix/lib/syncio/wrapper.h', 'src/starnix/lib/syncio/src/zxio.rs')
