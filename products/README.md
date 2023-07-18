# Fuchsia product definitions

This directory contains definitions for a variety of product configurations. The
products are organized into tracks that build on one another to produce a fully
featured system.

## Baseline track

### Bringup

A minimal system used to exercise kernel and core drivers and to bring up new
boards.

### Core

Self-updating system with core system services, connectivity, and metrics
reporting.

Builds on [bringup](#bringup)

## Workstation track

### Terminal (deprecated per [RFC-0220])

A system with a simple graphical user interface with a command-line terminal.

Builds on [core](#core).

[RFC-0220]: /docs/contribute/governance/rfcs/0220_the_future_of_in_tree_products.md
