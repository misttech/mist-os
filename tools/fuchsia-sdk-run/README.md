# Fuchsia SDK Tool Runner

`fuchsia-sdk-run` is a command line tool to help with switching between
different project contexts where sdks may differ. It will
try to figure out through filesystem and environment context where the
appropriate sdk for the current project lives and search that SDK for
the host tool you're trying to run and run it with the remainder of arguments.

## Usage

It has three modes for being used:

- When run as `fuchsia-sdk-run` directly, it will use the first argument as
the tool to run and the remainder as arguments to it. Run this way it can only
use context cues to find the appropriate sdk. This is really only here to make
testing the binary easier, and to make it work normally if found and run. It's
not expected that users will run it this way.
- When run as `ffx` (through a symlink or hard link to this binary for example),
it will interpret the arguments as if it were `ffx` to allow you to specify
things that might help with finding the sdk (such as by passing
`-c sdk.root=/path/to/sdk`). It will then find ffx in the sdk and run it with
the same arguments.
- When run as anything else, it will act the same way as if you ran it as
`fuchsia-sdk-run` but using `argv[0]` as the tool name and the normal arguments
as the arguments to that tool.

In all of the above it will exit with the exit code of the tool being run,
or `127` if the tool couldn't be found either because the SDK was not found or
the tool did not exist in the SDK.

## Setup

You can set up a directory of symlinks from the names of tools that exist in
the sdk to the `fuchsia-sdk-run` binary and add that directory to your `PATH`
environment variable to make it so you can access those tools from anywhere,
using the correct SDK for your project.

For example:

```bash
mkdir ~/fuchsia-sdk-run
ln -s ~/fuchsia/out/host-tools/fuchsia-sdk-run ~/fuchsia-sdk-run/ffx
ln -s ~/fuchsia/out/host-tools/fuchsia-sdk-run ~/fuchsia-sdk-run/zxdb
ln -s ~/fuchsia/out/host-tools/fuchsia-sdk-run ~/fuchsia-sdk-run/far
export PATH="~/fuchsia-sdk-run:${PATH}"
ffx --help
```

This will make it so that you can run `ffx` (including `ffx sdk run`) through
the "ffx" mode and `zxdb` and `far` through the "default sdk" search method,
finding them in the SDK and running them that way.

### Alias based Setup

Common shells (e.g. bash, zsh, fish) support aliasing one command to another.

Instead of hardlinking the `fuchsia-sdk-run` binary to `ffx` you could Instead
add `fuchsia-sdk-run` to your path, then put the following in your shell 
setup files (e.g. `.bashrc` or `.zshrc`):

```bash
alias ffx='fuchsia-sdk-run ffx'
```

This will allow you to run `ffx target list` from your prompt, and the actual
command which will be run is:

```bash
fuchsia-sdk-run ffx target list
```
