# Xephyr scenario tests

The scenario harness runs this bspwm implementation in a private Xephyr server. It owns the X server, daemon, test clients, socket, state file, and temporary directory, and cleans them on success, failure, or interruption.

## Prerequisites

- A POSIX shell
- Rust and Cargo
- Xephyr
- `jq`
- `xdpyinfo`, `xwininfo`, `mktemp`, and standard POSIX utilities

On Debian or Ubuntu, the non-Rust dependencies are provided by `xserver-xephyr`, `jq`, and `x11-utils`. The tests do not use `jshon`.

## Running

Run every executable scenario:

```sh
tests/run
```

Run one or more scenarios by basename or path:

```sh
tests/run 01-manage-close-state 05-preselection-receptacle
tests/run tests/scenarios/99-restart-preserves-client
```

The runner builds `bspwm-rs`, `bspc-rs`, `examples/test_window.rs`, and `examples/x_pixel.rs` before starting. Every wait has a bounded deadline; an outer CI timeout is still recommended, for example `timeout 90s tests/run`.

## Debugging

Set `KEEP_TEST_ENV=1` to preserve the temporary directory and logs after all child processes and sockets have been cleaned up:

```sh
KEEP_TEST_ENV=1 tests/run 01-manage-close-state
```

The directory contains `xephyr.log`, `bspwm.log`, `test-window.log`, subscription output, and per-window XID files. Its path is printed during cleanup.

For manual interaction, set `PAUSE_TEST_ENV=1`. Before cleanup the runner prints `DISPLAY` and `BSPWM_SOCKET` and waits for Enter. In another terminal, export those values and use `target/debug/bspc-rs`, or start an X client on that display. Combine it with `KEEP_TEST_ENV=1` when logs should remain after cleanup.

Test windows receive rotating background colors so multiple managed clients are easy to distinguish. Scenarios can request a specific color with the optional third argument:

```sh
red_window=$(add_test_window red DebugWindow '#D1495B')
blue_window=$(add_test_window blue DebugWindow '#30638E')
```

For manual debugging, the example accepts the same arguments directly and shows them in its title:

```sh
DISPLAY=:100 target/debug/examples/test_window demo DebugWindow '#2A9D8F'
```

`x_pixel` samples the root window through the X `GetImage` request and reports `#RRGGBB`. Scenarios use it to verify visible gaps and borders without screenshot tooling:

```sh
DISPLAY=:100 target/debug/examples/x_pixel 100 100
```
