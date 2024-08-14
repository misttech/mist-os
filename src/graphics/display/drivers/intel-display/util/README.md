# Display Driver Utilities

This directory contains building blocks that have proven useful across multiple
display drivers, but haven't had their APIs frozen yet. We aim to keep these
utilities in sync across all display drivers.

New functionality should only be added after it is used by multiple display
drivers. New functionality must have a narrowly scoped API and thorough test
coverage.
