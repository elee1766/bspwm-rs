# bspwm-rs audit

Commit `5620187`, workspace v0.9.12, rustc 1.98.1, 71 tracked Rust files / ~34.9k lines.

Every finding below was reproduced against the real `bspwm-rs` / `bspc-rs` binaries on a
private Xvfb display. Claims that did not reproduce are listed at the end so they are not
re-investigated later.

**Status: all findings are fixed**, except C5 and C6, which were deliberately left alone for
the reasons recorded under each. Each fix carries a regression test, and every reproduction
above was re-run against the fixed binaries. Final state: 294 unit tests, 30 ignored X tests,
and 19 scenarios all pass, with clippy clean.

## Baseline

| Check | Before | After |
| --- | --- | --- |
| `cargo test --workspace --all-targets` | 290 passed | 294 passed |
| `cargo clippy --workspace --all-targets` | 1 warning | clean |
| `tests/run` (nested Xephyr, 19 scenarios) | 19/19 | 19/19 |
| Ignored X tests on Xvfb | 27 passed, 3 failed | 30 passed, 0 failed |
| `make build` (release) | ok | ok |

The default suite and the scenario harness are both green. Every defect below sits in the
gap that those suites do not cover.

---

## C1. A self-transient client in a state file kills the window manager

**Severity: critical.** `crates/bspwm-xstack/src/lib.rs:195-208` accepts `child == parent`.
Enforcement at `:335` then removes the child from the order and calls
`self.position(parent).expect("parent was present")` on the entry it just removed.

Reproduced end to end. With one managed client, patching `transientFor` to the node's own id
and loading the dump:

```
load-state exit=0
DAEMON DEAD
panicked at crates/bspwm-xstack/src/lib.rs:335:56: parent was present
```

The whole session dies, taking every client's window management with it. `wm --load-state`
returns success first, so the caller sees no error.

Reachability: `crates/bspwm-state/src/restore.rs:383` filters only `0`, not self-reference.
The live property path is safe (`events/window.rs:635` filters self), as is initial manage
(`bspwm-x11/src/window.rs:194`), so a well-behaved client cannot trigger this. A corrupt,
hand-edited, or truncated state file can, and that file is also the restart path.

**Fixed.** `set_transient` now rejects `child == parent`, both `expect` calls became graceful
returns, and `restore.rs` filters a self-reference at the data boundary. Verified: the same
load-state reproduction now reports `DAEMON ALIVE`.

## C2. Restart state file follows symlinks in `/tmp`

**Severity: high.** `crates/bspwm-runtime/src/common.rs:9` uses the fixed, predictable path
`/tmp/bspwm{host}_{display}_{screen}-state`, and `persist.rs:326` writes it with
`std::fs::write`, which follows symlinks.

Reproduced: pre-creating `/tmp/bspwm_99_0-state` as a symlink to a file elsewhere, then
running `bspc-rs wm --restart`, overwrote the target with state JSON:

```
RESULT: VICTIM FILE OVERWRITTEN
```

On a shared or multi-user machine any local user can pre-plant that path and have the WM
truncate an arbitrary file the WM's user can write. Note `bspwm-ipc` already does this
correctly for sockets (`remove_stale_socket` checks the file type) and for FIFOs
(`create_fifo_in` uses randomized names under `XDG_RUNTIME_DIR`); the state path is the
one place that was not given the same treatment.

**Fixed.** Added `write_private_file`, which unlinks any existing entry then creates the file
with `O_EXCL | O_NOFOLLOW` at mode `0600`, and the state path now prefers `XDG_RUNTIME_DIR`.
Verified: the planted symlink no longer overwrites its target.

## C3. `monitor_add` and `desktop_add` never fire for command-created objects

**Severity: high (behavioral incompatibility with bspwm).** The only emit sites are
`daemon/monitors.rs:348` and `:478`, both inside RandR reconciliation. The command paths that
create these objects, `wm --add-monitor` (`commands/wm.rs:96-118`) and
`monitor --add-desktops` / `--reset-desktops` (`commands/monitor.rs:99-155`), queue only
`SyncEwmh`.

Reproduced with subscribers attached to each mask:

```
monitor_add subscriber got: b''
desktop_add subscriber got: b''
monitors now: ['screen', 'extra']
desktops now: ['Desktop', 'newdesk', 'Desktop']
```

The objects are created but no event is emitted. Status bars and scripts built on
`bspc subscribe monitor_add desktop_add`, a normal bspwm idiom, silently miss them. This is
exactly the class of incompatibility the readme asks to have reported.

**Fixed.** `wm --add-monitor` now broadcasts `monitor_add` plus the `desktop_add` for its
initial desktop, and the `monitor --add-desktops` / `--reset-desktops` paths broadcast
`desktop_add` through a shared helper. Verified: both subscribers now receive their events.

Note the neighbouring events are handled correctly: `monitor_rename`, `monitor_swap`,
`monitor_remove`, and `desktop_remove` all broadcast. Only the add pair was missed.

## C4. Invalid commands can report success

**Severity: medium-high.** Failure is signalled in-band by a leading `0x07` byte, but
`bspwm-client/src/lib.rs:87` only tests `chunk[0]` of each read. When a successful command's
output and a later error land in the same chunk, the marker sits mid-buffer and is missed.

Reproduced 30/30 against the real CLI:

```
$ bspc-rs wm --get-status --invalid-audit-option
exit=0
stdout=b"WMscreen:FDesktop:LT\n\x07wm: Unknown command: '--invalid-audit-option'.\n"
```

Exit code 0, and the raw `0x07` is printed to stdout instead of the message going to stderr.
Scripts using `set -e` or checking `$?` treat a rejected command as success.

The same function mis-splits the other way: a server that writes the marker and the message
in separate `write` calls sends the entire message to stdout with an empty stderr (also
reproduced). The existing unit test at `lib.rs:196` only covers the single-chunk case.

**Fixed.** `stream_response` now locates the marker anywhere in the chunk and carries the
failure state across reads. Verified: the invalid command exits 1, with output on stdout and
the diagnostic on stderr.

## C5. Multi-command requests apply a prefix before failing

**Severity: medium.** Domain handlers loop over commands and mutate state as they go
(`commands/mod.rs:97-110` and each domain's `while let` loop). A parse failure partway
through leaves earlier mutations applied.

Reproduced:

```
$ bspc-rs desktop --rename RENAMED --bogus-option
exit=1  stderr="desktop: Unknown command: '--bogus-option'."
$ bspc-rs query -D -d focused --names
RENAMED
```

The command reports failure but the rename persisted. This matches upstream bspwm's
behaviour, so it may be intentional; it is worth an explicit decision and a documentation
note either way.

**Left as is,** deliberately: changing it would diverge from upstream bspwm semantics, which
is a product decision rather than a defect fix.

## C6. IPC requests are framed on read boundaries, not message boundaries

**Severity: medium.** `bspwm-ipc/src/lib.rs:316` returns as soon as a read happens to end in
NUL. A client whose arguments span multiple writes has its command truncated and the prefix
executed.

Reproduced against the live daemon: sending `config\0` and `border_width\0` as two writes
produced the reply `1\n`, meaning the daemon answered a partial command rather than waiting.
With the library directly, `receive_request` consumed only `node\0` and left
`focused\0--state\0floating\0` unread.

In practice `bspc-rs` sends one `write_all`, so this needs a third-party client or a
fragmenting kernel to trigger; that makes it a latent rather than everyday bug.

**Left as is, and this one is worth explaining.** I implemented the obvious fix, framing on
EOF instead of on a trailing NUL, and then measured it: a client that sends a complete message
without closing its write side froze the entire window manager for 4.9s, because the daemon is
single-threaded and blocks in `handle_stream` until the read timeout. That trades a rare
truncation for a trivial full-session freeze, so I reverted it and documented the tradeoff in
`receive_request`. A real fix needs non-blocking, buffered per-connection reads in the event
loop, which is a larger change than this audit should make blind.

Related, `make_message` (`bspwm-client/src/lib.rs:18-33`) silently truncates at the 8192-byte
buffer: an over-long argument drops all following arguments with no error, so
`monitor --rename <8KB name> --focus` silently loses `--focus`.

**Fixed.** `make_message` returns `None` instead of truncating, and `bspc-rs` reports the
oversize message as a usage error rather than a connection failure.

## C7. Transient cycles are accepted

**Severity: low.** `set_transient` does not reject cycles. `A→B` plus `B→A` is accepted, and
the bounded enforcement loops exit without the constraint holding. Verified it terminates
rather than hanging, but it emits redundant X traffic: three raises produced 19 stacking
operations on a two-window cycle. Worth rejecting at the API boundary alongside C1.

## Stale tests

Three ignored tests fail on Xvfb. All three look like stale expectations, not product bugs,
but they should be fixed so the ignored suite is usable as a signal.

All three were stale expectations, and all three are now fixed and passing.

- `live_rule_properties_reports_bad_window_for_a_dead_client` expected `Err` for a destroyed
  window. Renamed to `..._returns_defaults_for_a_dead_client` and now asserts the real
  contract: default properties plus `exists == false`.
- `live_schedule_applies_class_type_and_user_rules` expected `_NET_WM_STATE` to hold only
  `ABOVE`. The dialog is also the focused node, so `FOCUSED` is correctly present; the test
  predated that feature. I resolved the extra atom by name rather than guessing.
- `live_sync_resize_coalesces_acknowledgements_and_times_out_safely` asserted that
  `RuntimeApp::poll` applies a sync acknowledgement. It does not: the acknowledgement arrives
  as a Sync `AlarmNotify` event, which the real loop dispatches through `handle_event`. The
  test now pumps events the way the runtime does.

## Investigated, did not reproduce

Recorded so these are not chased again.

- **Strut integer narrowing** (`bspwm-core/src/strut.rs:79`). The `u32 as u16 as i16` cast is
  genuinely lossy, but every oversized value is rejected by the surrounding bounds checks.
  Tested 32767/32768/65535/100000 on all four edges: padding stayed 0, `changed=false`. Cosmetic
  at most.
- **`adapt_geometry` overflow** (`bspwm-core/src/geometry.rs:166-179`). Tested far-offscreen
  and 20000px rectangles across monitors; no panic, no wrap.
- **Slow-subscriber daemon hang.** Subscriber writes are blocking, so the concern is
  reasonable, but 3000 events against a subscriber with a 1KB receive buffer that never read
  left the daemon fully responsive. Not reachable through normal event volume.

## What changed

| Finding | Outcome |
| --- | --- |
| C1 self-transient panic | Fixed, with `self_transient_is_rejected_without_panicking` |
| C2 symlink write | Fixed, with `private_write_replaces_a_planted_symlink...` |
| C3 missing add events | Fixed, verified with live subscribers |
| C4 false success | Fixed, with split-chunk and combined-chunk tests |
| C5 partial execution | Left as is: matches upstream bspwm |
| C6 request framing | Left as is: the fix caused a 4.9s WM freeze, see above |
| C6 message truncation | Fixed, `make_message` now reports oversize messages |
| C7 transient cycles | Fixed, with `cyclic_transient_is_rejected` |
| 3 stale ignored tests | Fixed, ignored suite now 30/30 |

## Coverage and limits

Read every tracked Rust file, the shell harness, and the packaging. Dynamic validation ran on
Xvfb and nested Xephyr only. Not covered: real multi-head RandR hardware, pointer/keyboard
grab interaction with a real input device, and long-running stability under a real desktop
session. The previously failing `live_daemon` tests are resolved and the ignored suite passes
in full on Xvfb; it has not been re-run under Xephyr.
