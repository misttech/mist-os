// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_LIB_SYNCIO_STUB_MISSING_INCLUDES_H_
#define SRC_STARNIX_LIB_SYNCIO_STUB_MISSING_INCLUDES_H_

// Adding includes that are not detected by rust-bindings because they are
// defined using functions

#include <lib/zxio/types.h>

const zxio_shutdown_options_t _ZXIO_SHUTDOWN_OPTIONS_READ = ZXIO_SHUTDOWN_OPTIONS_READ;
#undef ZXIO_SHUTDOWN_OPTIONS_READ
const zxio_shutdown_options_t ZXIO_SHUTDOWN_OPTIONS_READ = _ZXIO_SHUTDOWN_OPTIONS_READ;

const zxio_shutdown_options_t _ZXIO_SHUTDOWN_OPTIONS_WRITE = ZXIO_SHUTDOWN_OPTIONS_WRITE;
#undef ZXIO_SHUTDOWN_OPTIONS_WRITE
const zxio_shutdown_options_t ZXIO_SHUTDOWN_OPTIONS_WRITE = _ZXIO_SHUTDOWN_OPTIONS_WRITE;

const zxio_node_protocols_t _ZXIO_NODE_PROTOCOL_NONE = ZXIO_NODE_PROTOCOL_NONE;
#undef ZXIO_NODE_PROTOCOL_NONE
const zxio_node_protocols_t ZXIO_NODE_PROTOCOL_NONE = _ZXIO_NODE_PROTOCOL_NONE;

const zxio_node_protocols_t _ZXIO_NODE_PROTOCOL_CONNECTOR = ZXIO_NODE_PROTOCOL_CONNECTOR;
#undef ZXIO_NODE_PROTOCOL_CONNECTOR
const zxio_node_protocols_t ZXIO_NODE_PROTOCOL_CONNECTOR = _ZXIO_NODE_PROTOCOL_CONNECTOR;

const zxio_node_protocols_t _ZXIO_NODE_PROTOCOL_DIRECTORY = ZXIO_NODE_PROTOCOL_DIRECTORY;
#undef ZXIO_NODE_PROTOCOL_DIRECTORY
const zxio_node_protocols_t ZXIO_NODE_PROTOCOL_DIRECTORY = _ZXIO_NODE_PROTOCOL_DIRECTORY;

const zxio_node_protocols_t _ZXIO_NODE_PROTOCOL_FILE = ZXIO_NODE_PROTOCOL_FILE;
#undef ZXIO_NODE_PROTOCOL_FILE
const zxio_node_protocols_t ZXIO_NODE_PROTOCOL_FILE = _ZXIO_NODE_PROTOCOL_FILE;

const zxio_node_protocols_t _ZXIO_NODE_PROTOCOL_SYMLINK = ZXIO_NODE_PROTOCOL_SYMLINK;
#undef ZXIO_NODE_PROTOCOL_SYMLINK
const zxio_node_protocols_t ZXIO_NODE_PROTOCOL_SYMLINK = _ZXIO_NODE_PROTOCOL_SYMLINK;

const zxio_seek_origin_t _ZXIO_SEEK_ORIGIN_START = ZXIO_SEEK_ORIGIN_START;
#undef ZXIO_SEEK_ORIGIN_START
const zxio_seek_origin_t ZXIO_SEEK_ORIGIN_START = _ZXIO_SEEK_ORIGIN_START;

const zxio_seek_origin_t _ZXIO_SEEK_ORIGIN_CURRENT = ZXIO_SEEK_ORIGIN_CURRENT;
#undef ZXIO_SEEK_ORIGIN_CURRENT
const zxio_seek_origin_t ZXIO_SEEK_ORIGIN_CURRENT = _ZXIO_SEEK_ORIGIN_CURRENT;

const zxio_seek_origin_t _ZXIO_SEEK_ORIGIN_END = ZXIO_SEEK_ORIGIN_END;
#undef ZXIO_SEEK_ORIGIN_END
const zxio_seek_origin_t ZXIO_SEEK_ORIGIN_END = _ZXIO_SEEK_ORIGIN_END;

#endif  // SRC_STARNIX_LIB_SYNCIO_STUB_MISSING_INCLUDES_H_
