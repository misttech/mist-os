SDK frontends
=============

This directory contains a frontend for the SDK pipeline:
- [`gn/`](gn): creates a C/C++ GN workspace.

In addition, the `common/` directory provides plumbing shared by all frontends, 
`tools/` contains various tools to work with SDK manifests, and `merger/` 
contains the merge script used to assemble multiple SDK platform subbuilds 
into the final product.


Build script
============

This directory also contains `build_sdk.py`, a script that can be used to 
build the full IDK locally equivalent to what's available 
on https://fuchsia.dev/fuchsia-src/development/idk. 

## Usage
```bash
# Provide the platform(s) you'd like to build for with --arch
# and the output filename. The output file is a tar.gz.
build_sdk.py --arch x64 arm64 --output ~/fuchsia_sdk.tar.gz

# Arch can be `x64`, `arm64`, or both. Use both to replicate the standard
# build process, or pick just the one you need for a much faster build time.
build_sdk.py --arch x64 --output ~/fuchsia_sdk.tar.gz

# --output-dir can be used instead if a compressed archive is not required.
build_sdk.py --arch x64 --output-dir ~/fuchsia_sdk/

# Use -GN to apply the GN build rules after the build.
# Googlers can use --internal to build the internal SDK instead,
# and --rbe to enable remote builds.
build_sdk.py --arch x64 --GN --internal --rbe --output ~/fuchsia_sdk.tar.gz
```

## Advanced Usage
It's also possible to manually compose and pass in fint param files. 
See `//tools/integration/fint/README.md` for more information about these 
configuration files as well as the relevant proto definitions. Note that 
flags that change the build configuration (e.g. --internal/--rbe) have no 
effect when directly using a fint param file.

Use `--fint-params-path` to pass in a single static config 
or `--fint-config` to pass in a json formatted string 
containing sets of static and context config files. See 
`//integration/infra/config/generated/fuchsia/fint_params/global.ci/` 
for examples of static configs used in CI. If provided,the context config 
used must include at a minimum the `checkout_dir` and `build_dir` fields.