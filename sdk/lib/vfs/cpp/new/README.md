# SDK C++ VFS

## Overview

This library provides basic pseudo-filesystem functionality, which can be useful
for exposing items in a component's outgoing namespace (including services).
This allows creation of pseudo-directories that can be modified at runtime,
pseudo/VMO-backed files, service connectors, and remote nodes.

Unlike the in-tree VFS (//src/storage/lib/vfs/cpp), this library is not intended
for filesystems development.

**WARNING**: This library is currently undergoing an API overhaul. Please avoid
new uses and prefer using the in-tree VFS at //src/storage/lib/vfs/cpp where
possible. See https://fxbug.dev/311176363 for details.

## Thread Safety

The node types this library implements are thread safe, however they must only
be used with a single-threaded asynchronous dispatcher. Multiple connections
may be created to a given node, as long as the same dispatcher is used.

Connections to a node are automatically closed when a node is destroyed. This
includes connections to child entries, if applicable, which were opened via a
parent node.

Use of a multi-threaded asynchronous dispatcher is **not** supported.
