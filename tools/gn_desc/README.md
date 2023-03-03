# `gn_desc`: A tool for rapidly querying the GN build graph

The `gn_desc` cli tool uses a pre-built copy of the JSON output of GN's `desc`
command to more rapidly query GN's build graph.

# Setup

The `gn_desc` tool is not built by default, to add it to your configuration:

`fx set ... --with-host //tools/gn_desc`

or add `//tools/gn_desc` to `host_labels` via `fx args`.

The tool can then be built with:

`fx build host-tools/gn_desc`

This also runs `gn` itself, again, to produce the `$outdir/gn_desc.json` file.

## Example Usage

To list targets that match a pattern:

`fx gn_desc -v --file out/default/gn_desc.json match <pattern> list`

The `<pattern>` is a regex.

# Usage

The cached `gn_desc.json` file can be rebuilt after changing GN files with:

`fx build host-tools/gn_desc`

If the GN files have been changed, a build hasn't been performed, the file is
not updated before running the tool.

Note: this can be adventageous when GN is failing and you need to determine what
the dependency edges are that are causing issues.

## Operation

The tool does the following when run:

1. parses the given file
1. creates a graph of all targets and their dependencies
1. selects some subset of the targets
1. runs a command on each selected target

## Target Selection

`gn_desc` has the following target selection mechanisms:

- `match <regex>` - selects all targets that match the given regex
- `match-file [-a|-ois] <regex>` - selects all targets that have files that
match the given regex:
  - `-a` - any file (the default)
  - `-i` - inputs
  - `-o` - outputs
  - `-s` - sources
  - `-x` - scripts (executable things)
- `label <label>` - Selects only given label (via an exact match, including the
toolchain).

## Commands

`gn_desc` has the following commands that can be run on the selected targets:

- `list` - lists the labels of the selected targets, one per line
- `summarize` - provides a summary description of the targets, which can be
customized by various flags, see the tool's own help for more information: `gn_desc --file <file> exact <label> summarize --help`

# Future Work

Planned future commands are:

- `dep-tree` - prints an ascii-art or dot-file format dependency tree
- `file-tree` - prints an ascii-art or dor-file format file input->output tree
- `metadata query` - performs a metadata-query similar to what `gn` does

Planned future selectors are:

- `match-metadata` - selects all targets that have a given metadata key




