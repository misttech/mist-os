# mist-os

## What is mist-os?

A POSIX-like operating system, designed to run unmodified Linux application targeting Cloud/HPC enviroment.
It's based on Zircon Kernel from Fuchsia/LK project.

[Read more about Fuchsia](https://fuchsia.dev).

[Read more about LK](https://github.com/littlekernel/lk).

## Set-up

_NOTE_: As derived from Fuchsia some scripts/tools are still necessary (plan to remove in the future).

```
# Jiri Bootstrap:
curl -s "https://fuchsia.googlesource.com/jiri/+/HEAD/scripts/bootstrap_jiri?format=TEXT" | base64 --decode | bash -s mist-os

# Update binary/packages dependencies using jiri
cd mist-os
export PATH=$PWD/.jiri_root/bin:$PATH

# Clone the REPO
jiri import -name=integration flower https://github.com/misttech/integration
jiri update

# Avoid jiri to update the project(git). It will be usefull for updating packages and deps (submodules).
jiri project-config --no-update=true --no-rebase=true

# Check out the main branch
git checkout main

# Build the kernel
make it

# Build and run qemu
make it rain

# Build and run some tests
make test
```

## Repo

The repo is a clone of Fuchsia original Repo from [here](https://fuchsia.googlesource.com/fuchsia).
The Fuchsia original files are preserved with .fuchsia extension

## License

Most of the code is governed by BSD-style licenses. Some parts are under an MIT-style license.

[BSD-style](LICENSE)

The Third Party Components (third_party/...) are under various
licenses (of the BSD, MIT, or Zlib style), which may be found in the
root directory of each component, respectively.
