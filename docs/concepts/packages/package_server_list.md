# Listing running package servers

## Overview

The package repository server is controlled by running the subcommands of
[`ffx repository server`](/reference/tools/sdk/ffx#server). This server handles
 requests for metadata about the available packages and delivers the file blobs
 that make up the package contents.

There can be multiple package servers running on a single host machine. It is
useful to be able to list these running servers and their properties. These
properties are useful for troubleshooting or integrating package servers into a
higher level system or workflow automation.

## Basic command

```posix-terminal
ffx repository server list
```

This command supports the [top level ffx options](/reference/tools/sdk/ffx) to
support programmatic use of the command:

* `--machine` produces machine reader friendly output
* `--schema` produces the JSON schema of the machine output

## Options

### `--full`

Produces the full details for each running package server.

### `--name`

Limit the output to package servers with the given name.
This option can be specified multiple times.

## Output

The default output is a list of:

* name
* listening address
* repo_path

```none
devhost      \[::\]:8083   /path/to/product_bundles/core.x64/repository
```

Full output adds additional fields:

* execution mode
* registration_aliases
* registration_storage_type
* registration_alias_conflict_mode
* pid

## Examples

### Listing all running servers

```posix-terminal
ffx repository server list
```

### Listing full details of all running servers

```posix-terminal
ffx repository server list --full
```

### Listing all details for the server named `devhost` or `devhost2`, JSON encoded

```posix-terminal
ffx --machine json-pretty repository server list --name devhost --name devhost2
```
