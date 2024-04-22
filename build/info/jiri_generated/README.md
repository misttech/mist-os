This directory contains two files (`integration_commit_hash.txt` and
`integration_commit_stamp.txt`) which reflect the commit hash and timestamp of
`//integration/.git/HEAD` respectively.

These files are cannot be checked into git in any state since there is no way to
`.gitignore` files that are already tracked by git. Rather, a jiri hook in
`//integration/fuchsia/stem` is used to invoke
`//build/info/create_jiri_hook_files.sh`, which will create these on each
`jiri sync`/`jiri update` operation with up-to-date values.

As such, fallback values/mechanisms should be used before the jiri hook is
established.

For context, see https://fxbug.dev/335391299
