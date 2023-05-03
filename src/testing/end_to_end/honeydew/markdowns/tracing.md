# Tracing affordance

[TOC]

This page talks about [Tracing] affordance in HoneyDew.

## Usage
```python
>>> emu.tracing.initialize()
>>> emu.tracing.start()
>>> emu.tracing.stop()
# call terminate when we want to terminate the trace session without saving the trace.
>>> emu.tracing.terminate()
# call terminate_and_download when we want to terminate the trace session and save the trace.
>>> emu.tracing.terminate_and_download(directory="/tmp/")
```

[Tracing]: ../interfaces/affordances/tracing.py
