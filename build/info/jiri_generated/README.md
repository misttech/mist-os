This directory contains two files which reflect the commit
hash and timestamp of //integration/.git/HEAD.

A Jiri hook, in //integration/fuchsia/stem, is used to invoke
`//build/info/create_jiri_hook_files.sh` which will overwrite
them, on each `jiri sync` operation, with up-to-date values.

The files are listed in //.gitignore to ensure that their
updates are properly ignored by git commands. Their default
values in the fuchsia.git checkout are provided to ensure
that everything works even if the hook is not enabled yet in
the integration.git repository.

For context, see https://fxbug.dev/335391299
