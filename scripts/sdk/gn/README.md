# GN SDK

The GN SDK frontend produces a [GN](https://gn.googlesource.com/gn/+/HEAD/README.md) workspace.
Note that this directory has been deprecated. The GN SDK will be removed soon.
Changes made to this directory will no longer take effect. Further changes
should be made to
https://chromium.googlesource.com/chromium/src/third_party/fuchsia-gn-sdk/
instead.

## Directory structure

### Generation files & folders
- `generate.py`: the script that generates the SDK
- `BUILD.gn`: GN build rules to build & test the GN SDK and generation process
- `templates`: Mako templates used to produce various SDK files
- `base`: SDK contents that are copied verbatim

### Testing files & folders
- `test_generate.py`: script to test the GN SDK generation process
- `update_golden.py`: script to update the contents of the golden directory
- `host_test.go`: Go script to run test defined in GN build rules
- `testdata`: files used as input during tests
- `golden`: files used to verify generator output during tests
- `test_project`: GN project used to test building a projects with the GN SDK
- `bash_tests`: contains test for bash scripts in base/bin

## Generating

1. Generate a IDK/core SDK:

   ```
   fx set minimal.x64
   fx build sdk:final_fuchsia_idk
   ```

1. Run the generate script:

   ```sh
   $ fuchsia-vendored-python scripts/sdk/gn/generate.py \
       --archive out/default/sdk/archive/fuchsia_idk.tar.gz \
       --output gn_sdk_dir
   ```

### Testing

Note: many of these tests require `${FUCHSIA_DIR}/.jiri_root/bin` to be on `$PATH`. This might need to be added.

#### Execute GN SDK scripts/tools

The internal GN SDK helper scripts/tools can be executed after the GN SDK has been generated.

```sh
$ gn_sdk_dir/tools/x64/fserve
```

#### SDK generator tests

To test the generator, run the `test_generate.py` script.

```sh
$ fuchsia-vendored-python scripts/sdk/gn/test_generate.py
```

This runs the generator against the `testdata` directory and compares the output
to the files in the `golden` directory.

After making changes to the generator, update the contents of `testdata` as
needed to exercise your new code, then run the `update_golden.py` script to fix
the `golden` files.

```sh
$ fuchsia-vendored-python scripts/sdk/gn/update_golden.py
```

Commit your changes to the generator, `testdata` contents, and `golden` contents
together.

#### Bash scripts test

Make sure the tests are part of your build by adding `--with //scripts/sdk/gn:tests` to your `fx set` command.

To test the bash scripts, run `fx test host_x64/gn_sdk_script_tests`


#### Test project test

To test the SDK on a test project, the `generate.py` must be run with the `--tests` flag. `generate.py` requires a IDK (integrator development kit) build

1. Download the latest IDK (integrator development kit) to a temporary
directory (assuming the current directory is $FUCHSIA_DIR):

   ```sh
   $ mkdir -p out/temp
   $ BUILD_ID="$(gsutil cat gs://fuchsia/development/LATEST_LINUX)"
   $ gsutil cp gs://fuchsia/development/$BUILD_ID/sdk/linux-amd64/core.tar.gz out/temp/idk.tar.gz
   ```
1. Generate the test workspace into a temporary directory:

   ```sh
   $ fuchsia-vendored-python scripts/sdk/gn/generate.py \
    --archive out/temp/idk.tar.gz \
    --output out/temp/gn_sdk_dir/ \
    --tests out/temp/test_workspace
   ```

1. Run the `run.py` file in the test workspace:

   ```sh
   $ fuchsia-vendored-python out/temp/test_workspace/run.py
   ```
