# gRPC Management in Fuchsia
The gRPC build configuration is managed in this directory in the Fuchsia source tree (`fuchsia.git`), while the gRPC source code lives in Fuchsia's [mirror repository] of the [upstream gRPC repository]. The [gRPC GI pin] is meant to point to an upstream gRPC commit hash, i.e. a commit hash on upstream/main of the mirror, and not a mirror-specific commit hash.

# Uprev gRPC in Fuchsia
Upreving gRPC requires updating `BUILD.gn` in this directory to align with the target version and updating the [gRPC GI pin]. However, these updates may require breaking changes as some gRPC updates require build changes and the code span two repositories. To avoid making a breaking change, we will leverage the main branch of the mirror repo:
1. Patch the updated `BUILD.gn` and merge the change from target gRPC version to the main branch of the [mirror repository].

   Example:  https://fxrev.dev/1201532
2. Temporarily point the [gRPC GI pin] to the commit with both the updated `BUILD.gn` and source of gRPC of target version in the main branch of the [mirror repository].
3. Merge the updated `BUILD.gn` into `fuchsia.git`.

   Example: https://fxrev.dev/1201685
4. Point [gRPC GI pin] to the upstream target gRPC commit hash again.

The process involves two main steps: Update `BUILD.gn` file tailored for the target gRPC version in `fuchsia.git` (described in the first section below) and updating the [gRPC GI pin] (described in the second section below).

## Update BUILD.gn for the target version of gRPC
Adapted from [Chromium].

This should be done in `//build/secondary/third_party/grpc`.
1. Update the template:
   ```
   curl -sfSL \
     https://chromium.googlesource.com/chromium/src/+/main/third_party/grpc/template/BUILD.chromium.gn.template?format=TEXT | \
     base64 --decode | sed -E "s/([\"'])src/\1./g" > template/BUILD.fuchsia.gn.template
   ```
1. Apply Fuchsia-specific patch:
   ```
   patch -d $FUCHSIA_DIR -p1 --merge < template/fuchsia.patch
   ```
1. Resolve conflicts.
1. Commit the result.
1. Regenerate Fuchsia-specific patch:
   1. Download the original template (see curl above).
   1. Generate the patch:
      ```
      git diff -R template/BUILD.fuchsia.gn.template > template/fuchsia.patch
      ```
   1. Restore the template:
      ```
      git checkout template/BUILD.fuchsia.gn.template
      ```
1. Update `$FUCHSIA_DIR/integration/fuchsia/third_party/flower` to reference the target gRPC revision.
1. Install prerequisites:
   ```
   sudo apt install python3-mako
   ```
1. Rebuild `BUILD.gn`:
   ```
   git -C $FUCHSIA_DIR/third_party/grpc submodule update --init
   cp template/BUILD.fuchsia.gn.template $FUCHSIA_DIR/third_party/grpc/templates
   (cd $FUCHSIA_DIR/third_party/grpc && tools/buildgen/generate_projects.sh)
   rm $FUCHSIA_DIR/third_party/grpc/templates/BUILD.fuchsia.gn.template
   mv $FUCHSIA_DIR/third_party/grpc/BUILD.fuchsia.gn BUILD.gn
   fx gn format --in-place BUILD.gn
   git -C $FUCHSIA_DIR/third_party/grpc submodule deinit --all
   ```

## Updating gRPC GI pin to the target version
Workaround to avoid breaking change when updating gRPC GI pin:
1. After `BUILD.gn` is updated in the previous section, merge changes from the target version and patch the updated `BUILD.gn` from `//build/secondary/third_party/grpc` to `//third_party/grpc`:
    ```
    cd $FUCHSIA_DIR/third_party/grpc
    git checkout origin/main -b ${USER}-grpc-update
    git merge -X theirs <target-version-commit-hash>
    cp $FUCHSIA_DIR/build/secondary/third_party/grpc/BUILD.gn BUILD.gn
    ```
1. After merging the changes to main branch of the [mirror repository], i.e the `//third_party/grpc` CL is reviewed and submitted on Gerrit, pin the [gRPC GI pin] to the commit hash of the merge commit created in the previous step.
1. Merge the updated `BUILD.gn` file into `fuchsia.git` because gRPC source has advanced now.
1. Pin the [gRPC GI pin] to the upstream target gRPC commit hash.

[Chromium]: https://source.chromium.org/chromium/chromium/src/+/main:third_party/grpc/README.chromium
[mirror repository]: https://fuchsia.googlesource.com/third_party/grpc/
[gRPC GI pin]:  https://turquoise-internal.googlesource.com/integration/+/aec8c717eb937f469893e01f3a63537bbae21088/fuchsia/third_party/flower#278
[upstream gRPC repository]: https://github.com/grpc/grpc
