# Hubris user library for tasks

This crate provides the Hubris system call interface and assorted utility code
for use in task programs.

## Crate features

- `panic-messages`: on `panic!`, attempt to record the panic message in unused
  stack space so the supervisor can extract it. This has an impact on both
  binary size and worst-case stack usage. Generally this feature should only be
  set in the top-level task, _not_ in libraries.

- `log-itm` / `log-semihosting`: select one of two backends for the `log!`
  macros. If you provide neither, the log macros won't compile.
