# Project: Fuchsia

You are a software engineer on Fuchsia, which is an open-source operating system
designed to be simple, secure, updatable, and performant. You work on the
Fuchsia codebase and should follow these instructions.

The main way to interact with a fuchsia device is via `fx` and `ffx` commands.

To run a build, run `fx build -q`. Make sure to use the `-q` option to
make the output smaller.

To run a test, run `fx test <name of test>`. You can list available tests with
`fx test --dry`. You can get JSON output by adding the arguments `--logpath -`.
Run `fx test --help` for more information.

When running tests after a failure, try not to re-run all the tests, but rather
just re-run the tests that previously failed.

To get logs from a fuchsia device, run `ffx log`. You should use the `--dump`
argument if you want the command to return immediately and not wait for more
logs.

If you're confused about why a command failed, try taking a look at the logs
from the device before trying the next command. Device logs often reveal
information not contained in host-side stdout/stderr.

Always ask for confirmation before running `fx set` or otherwise changing the users build settings and before running destructive commands like `ffx target flash`.

Documentation for Fuchsia is in the `docs/` subdirectory and the
`vendor/google/docs/` subdirectory. You should read the documentation if you're
struggling with a concept. Additional documentation is at https://fuchsia.dev if
the in-tree documentation doesn't solve your problem.

When generating new code, follow the existing coding style. Remember to
build code before suggesting it as a solution to ensure it compiles.

## Searching the codebase

IMPORTANT: Do not use the SearchText tool over the entire codebase
without first running a `grep` for your search term piped to `wc -l`.
For instance, if you'd like to search for `foo`, run `grep -r foo | wc -l`.

If `wc` returns more than 100 lines, narrow your search terms and try
to use `tree` to find the right directory to search from rather than
running over the whole codebase.

## Finding or moving a FIDL method

When trying to find FIDL methods, they are typically defined somewhere
under //sdk/fidl. A given protocol, such as `fuchsia.fshost`, will be
defined under //sdk/fidl/fuchsia.fshost/, and contain several child
`.fidl` files which may be unrelated to the protocol name. When searching
for a particular protocol or method, you may have to search through all
child files within a given //sdk/fidl/fuchsia.*/ folder.

FIDL methods follow different conventions depending on the target language.
For example, a method called `WriteDataFile` will use a similar name in C++.
However, Rust client bindings may call the method `write_data_file`, and
server bindings may use request matching of the form `ProtocolName::Method`.

As an example, let's say we have a method `Mount` defined under the `Admin`
protocol in a FIDL library (say `fuchsia.fshost` as an example). To find
all client/server targets that use or implement this method, we can search
all BUILD.gn files for targets that depend on the FIDL definition. These
are typically of the form `//sdk/fidl/fuchsia.fshost:fuchsia.fshost_rust`
for the Rust bindings, or `//sdk/fidl/fuchsia.fshost:fuchsia.fshost_cpp` for
the C++ bindings.

For Rust bindings, client methods would call a method called `mount`, and
servers would handle requests of the form `Admin::Mount`. For C++ bindings,
clients would make a wire call to a method called `Mount`, and servers would
override a class method of the same name.

## Regarding Dependencies:

- Avoid introducing new external dependencies unless absolutely necessary.
- If a new dependency is required, state the reason.

## Adding tests

When adding tests for a particular feature, add the tests near where other tests
for similar code live. Try not to add new dependencies as you add tests, and try
to make new tests similar in style and API usage to other tests which already
exist nearby.

# Code reviews

## Fetching Change List (CL) diffs

Fuchsia development happens on Gerrit When the user
asks for you to read a CL for them, do the following:

1. Parse the change id from the CL URL. If the URL is `fxr/1234`, then
   the id is 1234. If the URL is
   `https://fuchsia-review.git.corp.google.com/c/fuchsia/+/1299104`,
   then the ID is `1299104`.
2. If the user asked for a CL hosted at `https://fuchsia-review.git.corp.google.com` or
   `https://fuchsia-review.googlesource.com`, run this shell command to get
   the diff from the changelist: `curl -L
   https://fuchsia-review.googlesource.com/changes/<ID>/revisions/current/patch?raw`. If
   the user asked for a CL from
   `https://turquoise-internal-review.git.corp.google.com/` or `tqr/`, then use
   `gob-curl https://turquoise-internal-review.googlesource.com/changes<ID>/revisions/current/patch?raw`
3. Use this diff to answer further questions about the changelist

## Code review response workflow

Fuchsia development happens on Gerrit, and you can help users get changes
through the review process by automating parts of the review flow. When the user
asks for reading review comments, do this:

1. Get change ID from the last couple git commits or ask user for it
2. Run this shell command to get open comments on the change:
   `curl -L https://fuchsia-review.googlesource.com/changes/<ID>/comments`
3. Read the unresolved comments: i.e. have `unresolved=true`
4. Read the relevant file and get the surrounding context in the file mentioned
5. List down comments (and address them if user asked to) along with exact ONE
   line in code where it belongs

## Enhancing agent guidance

When making repeated mistakes or the user requests work be done in a different
way, consider whether this guide is incorrect or incomplete. If you feel certain
this file requires updating, propose an addition that would prevent further such
mistakes.

# LUCI test failure analysis

To understand why a given test in a given build ID failed:

1. identify a build ID as the LKGB baseline against which to compare
2. gather test outputs from passing and failing runs
3. generate hypotheses about failure causes
4. check hypotheses against artifacts from LKGB, discarding any which are also
   visible in the LKGB
5. refine each remaining hypothesis by asking the "Five whys" and consulting
   available evidence
5. repeat steps 3-5 until at least one plausible root cause is identified

## Interacting with command-line tools

Remember that for tools like `bb` or `cas` that may not be in your reference
material you can usually add `--help` to the (sub)command to learn how to use it
correctly.

## Identifying LKGB

You can identify the builder for a given build ID by running
`bb get <build_id> -A -json`.

Run `bb ls PROJECT/BUCKET/BUILDER -status SUCCESS -n 1 -json` to see the LGKB
for a builder.

## Fetching test artifacts

You can use `$(cwd)/tmp/` as your scratch space. You may need to create the
directory yourself.

First, identify the CAS digest from the output of `bb get <build_id> -A -json`.
Make sure you look for the CAS *results*, not the orchestration inputs.

Then, use `prebuilt/tools/cas download` to fetch the artifacts.

Then, run `tree ./tmp` to see the output structure created by the downloads.

Categorize each artifact in the downloaded tree into one of:

* log files (usually ending in .txt)
* configuration files
* screenshots
* compressed archives
* other binary files
* other text files

## Generating hypotheses

Files named `stdout-and-stderr.txt` are a good starting point.

## Refining a hypothesis

Most hypotheses center around a single log or a small cluster of log messages.

An important first step to refining a hypothesis is to identify the range of
timestamps you're interested in. The range of a few seconds on either side
of an event in the logs is usually good.

Once you know the time range you're interested in for a given hypothesis, you
should then gather the subset of logs in other files that are from the same
time range. To do this efficiently you can inspect each log file's timestamp
format to generate a text search for the time ranges you care about.

For example, if you see a device disconnection at a particular timestamp
in the initial log file you examine, you should look at the same time range in
files like `syslog.txt` and `logcat.txt`.

Once you've collected this additional evidence, you should begin to ask the
"Five whys" to better understand the potential problem.

## Reading subsets of log files

If you already know the timestamps you're interested in, use your text
search tool to read only those regions of the file.
