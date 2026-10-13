# The loop guard

A check of the machine code of silt's instruction loop
(`Vm::run_frames`, src/vm/run.rs), run by the `loop-guard` job of
`.github/workflows/ci.yml` on the release binary, built as
`release.yml` builds it with the compiler that builds the releases.

## What it guards

A silt Value is 40 bytes. When the loop writes a new one in two small
stores (its tag, its number) and then reads it back in 16-byte pieces,
as a move of the whole value does, the processor cannot serve the load
from its store buffer and waits for the stores, 12 to 14 cycles each
time. rustc 1.98 began to compile the loop that way where 1.96 had not:
the plain loop ran 36 to 41% slower in the released binary, with no
change in silt. Pull request #54 changed the loop so that a new value
is written where it goes; the guard holds that.

Which code a compiler makes of the loop cannot be seen in a test of
what a program prints, and a wall time cannot be asserted. The guard
counts instead. It starts the binary on a loop, stops it in the loop,
steps 200,000 machine instructions (ptrace) and replays their loads and
stores. A round of the loop must have

- no load that overlaps a store of the last 60 executed instructions
  which does not cover it, and
- no more executed instructions than a bound (about 8% above what the
  compiler of 2026-10-12, rustc 1.99.0, makes: 557 for the plain loop,
  787 for the round with an add).

Both numbers are exact and do not depend on the machine's load.

The count of instructions is exact for one binary, but it is not a
property of the loop's source alone. When the rest of the crate
changes, the compiler builds the loop's one large function differently:
in October 2026 the same source read 539 and 557 instructions for the
plain loop (761 and 787 with an add) from one merge to the next. The
loads did not move once. So the first rule is the one that holds the
fix. The bound on instructions is there for a compiler that makes the
loop grossly longer; if it fails on a change that did not touch the
loop and the loads are still none, raise the bound.

## Running it

```
cargo build --release --target x86_64-unknown-linux-gnu
bash .github/scripts/loop-guard/run.sh target/x86_64-unknown-linux-gnu/release/silt
```

Linux on x86_64 only. It needs `cc`, `python3`, `nm` and `objdump`, and
leave to ptrace a child process. It takes about twenty seconds after
the build.

## When it fails

It prints each load that waits, with the stores it overlaps, and the
instructions per round by function. If the compiler changed and silt
did not, the listing shows which instruction's value is built somewhere
else again; `push_with` and the comment above it in src/vm/run.rs say
what the loop relies on. If the bound on instructions is passed and no
load waits, look at what the round runs now and raise the bound if that
is what is wanted.

## What it does not catch

- A slowdown that is neither a waiting load nor more instructions: a
  jump the processor predicts badly, code that falls on a worse
  address.
- A round it does not run: only a plain loop and one with an add are
  counted, not a call, a method call or a Float.
- Any other target. The releases for aarch64, macOS and Windows are
  built from the same source and are not looked at.
- What a real processor does. The rule for "cannot be forwarded" (an
  overlap without cover within 60 instructions) is a model; it was
  checked against time on one machine, an AMD EPYC 9354P.

## Files

- `step.c`: runs a program, stops it and steps it, writing how often
  each instruction ran and the registers before each step.
- `count.py`: makes the two numbers of a round from that and the
  binary's disassembly.
- `plain_loop.silt`, `add_round.silt`: the two loops.
- `run.sh`: builds `step`, runs both loops, applies the bounds.
