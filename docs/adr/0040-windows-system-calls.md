# ADR 0040: Win32 calls in one small `unsafe` crate

**Status:** Implementation decision, 28 November 2026
([issue #85](https://github.com/Langspan/LangSpan.dev/issues/85), slice (a)).

## Context

LangSpan's crates set `unsafe_code = "forbid"`, and the threat model says
LangSpan's own code is `unsafe`-free. On Unix, `rustix` makes the system calls
the daemon needs, such as `clock_gettime`, `geteuid` and no-follow opens, into
safe functions. A native Windows build needs Windows equivalents of two of
them, and both are Win32 calls with no safe wrapper in the standard library:

- **A sleep-inclusive monotonic clock.** `QueryInterruptTime` counts since
  boot and includes time spent in sleep and hibernation.
  `QueryUnbiasedInterruptTime` excludes it, so it is the wrong clock for
  `langspan::clock`, which promises to keep counting while the lid is closed.
  `std::time::Instant` documents no promise either way.
- **Owner-only files and directories**, the Windows counterpart of mode 0700
  and 0600. This means creating each one with a security descriptor, owned by
  the current user, whose protected DACL grants access only to that user and
  SYSTEM, and checking an existing one the same way.

`forbid` can't be lifted for one module. The options were a separate crate
that allows `unsafe`, changing `forbid` to `deny` with `allow` in one module,
or a third-party safe wrapper. No maintained wrapper covers both needs.

## Decision

**A workspace crate, `langspan-windows`, holds every Win32 call.** It depends
only on `windows-sys` (Microsoft's raw bindings, already in the lockfile
through `tokio` and others) and is empty on other platforms. `langspan`
depends on it only on Windows, and the `langspan`, `langspan-core` and
`langspan-graph` crates keep `unsafe_code = "forbid"`.

- Its API is safe and small: `interrupt_time_ms`,
  `create_private_directory`, `open_private_file`, `open_no_follow` and
  `is_owner_only`. No raw pointer or handle crosses it.
- Each `unsafe` block is one documented Win32 call with a `SAFETY` comment,
  enforced by `clippy::undocumented_unsafe_blocks`.
- Its tests are real: they create directories and files under the
  temporary directory and read their security descriptors back. An ordinary
  directory, which inherits its parent's entries, must be refused.

New Windows system calls (job objects for slice (b), the local transport
for slice (c)) go in this crate, behind the same kind of safe function.

## Consequences

- LangSpan's code is `unsafe`-free except for this crate. The threat model
  says so, and a review of `unsafe` reads one crate.
- `cargo deny` checks only the Unix and macOS release targets, so
  `windows-sys` is not licence- or advisory-checked there until a Windows
  target is added (slice (d)).
- The owner-only check is stricter than an ACL editor would show: an
  inherited entry, an unprotected DACL or any grant to another account
  refuses the path, as mode bits open to group or others do on Unix.
