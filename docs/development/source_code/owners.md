# Owners

Each file in Fuchsia has a set of owners. These are tracked in files
named `OWNERS`. One of these files is present in the root of the
repository, and many directories have their own `OWNERS` files too.

## Contents

Each `OWNERS` file lists a number of individuals (by their email address) who are
familiar with and can provide code reviews for the contents of that directory.

## Responsibilities

Fuchsia requires changes to have an `Code-Review +2` review, which anyone in the
'OWNERS' file can provide. In addition, many `OWNERS` files
contain a `*` allowing anyone to provide such a `+2`.

## Tools

Gerrit has a "suggest owners" button that will list all the owners for all the
files modified in a given change. More information on this is available on the
[Gerrit code-owners plugin][code-owners] page.

## Format

Fuchsia uses the [Gerrit file syntax][owners-syntax] for `OWNERS`
files.

Here's an example `OWNERS` file:

```none
# These users are owners
validuser1@example.com
validuser2@example.com

# Users listed elsewhere are also owners
include /path/to/another/OWNERS

# This user is only an owner of the listed file
per-file main.c = validuser3@example.com
```

## Best practices

*   It's important to have at least two individuals in an `OWNERS` file. Having areas
    of Fuchsia with a single owner leads to single points of failure. Having multiple
    owners ensures that knowledge and ownership is shared over areas of Fuchsia.
*   When applicable, `include` owners from another file rather than listing individuals.
    This creates fewer "sources of truth" and makes OWNERS maintenance easier.

## Owners override

In some cases, the author of a change may wish to override OWNERS
approval. This is appropriate primarily in cases where a change is
mostly mechanical but touches a large fraction of the codebase, for
example a trivial change to the signature of a commonly used API. CL
authors can add owners-override@fuchsia.dev to the reviewer list to
request owners override. This process should be used sparingly and
review by local owners is preferred whenever it does not present and
undue burden on developers.

[code-owners]: https://android-review.googlesource.com/plugins/code-owners/Documentation/index.html
[owners-syntax]: https://android-review.googlesource.com/plugins/code-owners/Documentation/backend-find-owners.html#syntax
