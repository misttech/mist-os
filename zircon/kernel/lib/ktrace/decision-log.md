
# On Target

```
] ktrace start 0xfff
... do something ...
] ktrace stop
] ktrace dump

```
Dump will be a [FXT](https://fuchsia.dev/fuchsia-src/reference/tracing/trace-format) hex and must start with `10 00 04 46 78 54 16 00.`

# On Host 

Copy the dump from the terminal to a text file ktrace.txt. Use the xxd to convert the text file to binary (fxt):

`
xxd -r -p trace.txt trace.fxt
`

Open [this](chrome://tracing/) and go to [new](https://ui.perfetto.dev/) perfetto ui to upload the file and make the analises.
