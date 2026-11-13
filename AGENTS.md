# AGENTS.md: working on DOSH

Read this before changing anything. It says how the project is built,
tested and extended, and how to work in small, checkable steps. For what
DOSH is and how it is structured, read [README.md](README.md) and
[docs/architecture.md](docs/architecture.md). For the current state and the
next step, read `docs/internal/HANDOFF.md` if it exists.

## Ground rules

- **Everything runs in the lab.** Host tools are limited to `git` and
  `podman`. Enter the lab with `./lab`, or prefix one command with it:
  `./lab make test`. Do not install toolchains on the host.
- **Acceptance is the proof.** A change is done when the acceptance suite
  passes on the real EFI artifact, with the evidence shown in the output.
  Compiling is not proof, and neither is a successful boot. Guest execution
  is proven only by a capsule's own `event=exit` line and its console output.
- **Never fake a DOS service.** An unimplemented call must fault the capsule
  with its name (`unimplemented service INT 21h ...`). Do not return a
  plausible value to make a program progress.
- **Keep the layers honest.** `crates/dos` is `no_std` and knows nothing
  about hardware, hypervisors or transports. `hv/` is the only code that
  knows the virtualization extension. `present.rs` is the only code that
  knows transports. Put behaviour in the layer that owns its invariant.
- **Do not push.** Commit locally. Pushing and opening PRs are the user's call.
- **Privacy.** Anything committed or written into a public file must not
  contain host paths, usernames, hostnames, IPs, serials, or tool-version
  fingerprints. Use repository-relative paths. Before committing, grep the
  tracked tree for the local username, your home-directory prefix, scratch
  directory prefixes and personal account names, excluding `.git`,
  `target`, `.lab` and `build`. The result should be empty.
- **Identity.** Commit as the project's identity only. Check with
  `git var GIT_AUTHOR_IDENT` before the first commit of a session.

## The loop

Work in increments you could return to. Each one is small enough to finish
in a sitting.

1. **Pick one behaviour.** Name it as an observable outcome: "a DOS program
   that calls X prints Y and exits 0". If the outcome cannot be observed in
   a capsule console or the supervisor log, it is not yet a behaviour.
2. **Find the failing evidence first.** Write or run the program or test
   that shows the gap. For DOS work, the gap is usually the fault line from
   a real program. Run it:
   `./lab tools/doshlab.py PROGRAM.COM`
3. **Make the smallest change in the owning layer.** Put DOS semantics in
   `crates/dos/src/int21.rs` (or the matching module), with a host unit test
   in `crates/dos/src/tests.rs`. Put hardware or scheduling changes in
   `crates/supervisor`. Reuse existing helpers (`Mem`, `Regs`, `Volume`,
   `Handle`) before adding new ones.
4. **Add the acceptance test.** Add a guest in `guests/` if the behaviour
   needs a program that does not exist yet, and an assertion in
   `tests/test_acceptance.py` on console bytes and exit code. Use the `dosh`
   fixture so the test runs on both `tcg` and `kvm`.
5. **Run the focused proof.** `./lab python3 -m pytest tests -k NAME` and
   `./lab cargo test -p dosh-dos`. Read the output.
6. **Run the gate.** `./lab make test`. It runs formatting, clippy with
   warnings denied, all unit tests and the full acceptance suite. All of it
   must pass before a milestone commit.
7. **Commit the checkpoint.** Commit when the tree is in a state you might
   want to return to. Say what the state is: `red: ...` if it is knowingly
   incomplete, `green: ...` if the gate passes. Do not commit unexpected red;
   revert it instead.
8. **Squash at the milestone.** When a roadmap item is complete, squash its
   checkpoints into one commit whose message describes the capability, not
   the story. Trailers: follow the repo's commit attribution rules.

Revert freely. If a change makes things worse, `git restore` or
`git reset --hard` back to the last green checkpoint is cheaper than
debugging the bad path. Only reset commits you made.

## Iteration style

- Prefer a program that exposes the gap over a unit test invented in the
  abstract. Real programs decide what the contract needs.
- Prefer one more real program over one more interrupt. Do not implement a
  whole family of services speculatively; implement what the next program
  demonstrably asks for, and say in the commit which program needed it.
- Keep a diff reviewable in one reading. If it touches three layers, check
  each layer's invariant still holds on its own.
- When an acceptance test is flaky, do not retry it into green. Find what
  is racing: usually input sent before a capsule's session banner, or a
  timeout that is too tight on the `tcg` path.
- Before calling something done, check it against the failure modes you can
  name: a guest that never exits, a guest that spins with interrupts off, a
  program that asks for a file that is absent, a full disk, a bad header.
  Add a test for any that is reachable.

## Where things are

| Need | Where |
| --- | --- |
| DOS semantics, errors, flags | `crates/dos/src/int21.rs`, `exec.rs`, `files.rs`, `arena.rs` |
| Interrupt vectors, BIOS data area | `crates/dos/src/firmware.rs` |
| Guest memory and registers | `crates/dos/src/mem.rs`, `Regs` in `lib.rs` |
| Capsule lifecycle, service gates | `crates/supervisor/src/capsule.rs` |
| Virtualization (SVM, world switch, exits) | `crates/supervisor/src/hv/` |
| Boot, manifest, drive C: provisioning | `crates/supervisor/src/boot.rs` |
| Paging, descriptor tables, traps, UART, clock | `crates/supervisor/src/platform/` |
| Console transports | `crates/supervisor/src/present.rs` |
| Guest programs | `guests/*.asm`, `guests/wc.c`, `guests/Makefile` |
| Acceptance tests | `tests/test_acceptance.py`, fixtures in `tests/conftest.py` |
| Test harness and QEMU wiring | `tools/doshlab.py` |
| Container | `tools/lab/Containerfile`, `lab` |
| Contract and design | `docs/dos-contract.md`, `docs/architecture.md` |
| Plan | `docs/roadmap.md` |

## Commands

```sh
./lab                                  # shell in the lab
./lab make efi                         # build the supervisor
./lab make guests                      # build the guest corpus
./lab make check                       # fmt, clippy, unit tests
./lab make test                        # everything
./lab python3 -m pytest tests -k NAME  # one acceptance test
./lab tools/doshlab.py --raw --keep build/run PROGRAM.COM   # diagnose a run
```

Build and check steps are plain make targets. Nothing needs a special
environment variable. Tests are non-interactive except `--attach`, which is
for people.

## Conventions

- Rust is stable 1.91, pinned in `rust-toolchain.toml` and the Containerfile.
  Change both together. `cargo fmt` and clippy with `-D warnings` are the
  style authority; do not hand-format around them.
- Assembly guests use NASM flat binary output, `org 0x100`, and comments
  that say what the program proves. Give each guest one purpose.
- Log lines are `key=value` with a subject first, so the test harness can
  parse them. Quote values with spaces. Keep the subject set small.
- Public names describe what a thing is, not how it is used today. Keep
  comments to the non-obvious: invariants, hardware quirks, why a DOS
  detail is shaped the way it is.
- Docs describe what exists. Put plans in `docs/roadmap.md`, not in code
  comments. Keep `docs/dos-contract.md` in step with the services.

## Things that have already bitten

- OVMF writes to every ISA serial port at boot. Capsule consoles therefore
  open with a banner, and input is held until that banner appears.
- The UEFI memory map includes high MMIO. `top_of_memory` must skip device
  regions, or the page tables grow past what one PML4 entry maps.
- INT 21h function 42h at origin 0 takes an unsigned offset. Other origins
  are signed.
- `./lab` must pass `-i` when stdin is piped, or the input never reaches the
  container.
- Shell heredocs that contain `shutil.rmtree` trip a harness safety check.
  Use the file-writing tool for such scripts.
- A guest `cli` plus a spin loop starves the supervisor unless the clock
  preempts it. Keep `V_INTR_MASKING` and the GIF handling in `svm.rs` intact.

## When you finish a session

1. Make sure `./lab make test` passes, or record exactly which part fails
   and why in the handoff.
2. Commit the checkpoint with an honest state line.
3. Update `docs/internal/HANDOFF.md` (untracked) with status, next step and
   how to pick up. That file is not committed.
4. Do not push.
