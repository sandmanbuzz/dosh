# DOSH

**Boot a machine. Offer it work. Run old software.**

DOSH is a small bare-metal supervisor that hosts DOS applications. It boots
as an EFI application on an x86-64 machine, takes the machine over from the
firmware, and runs each DOS program in its own *execution capsule*: a
hardware-virtualized real-mode guest whose instructions execute natively on
the processor. DOSH supplies what the program expects to find around it — a
DOS process environment, DOS and BIOS services, a console — and nothing
else. There is no host operating system underneath and no PC emulator
inside.

## What works today

- Boots under UEFI, leaves boot services, and runs on its own stack, page
  tables, descriptor tables and clock.
- Runs unmodified `.COM` and MZ `.EXE` programs under AMD SVM with nested
  paging. A guest that computes runs at native speed; it only leaves the
  processor to make a DOS or BIOS call.
- A first DOS contract: program loading and relocation, PSP and environment,
  command line, conventional memory management, console I/O (raw and
  line-edited), file handles on a per-capsule drive C:, exit codes.
  [docs/dos-contract.md](docs/dos-contract.md) is the precise list.
- Several capsules at once, isolated from one another and scheduled
  preemptively, each with its own console carried over its own serial port.
- A guest that misbehaves — an unknown DOS call, an unhandled exception, an
  attempt to leave real mode, a triple fault — stops its own capsule with a
  specific diagnostic. The supervisor and the other capsules carry on.

The test corpus is small assembly programs plus one C program built with a
compiler and C library from elsewhere. Nothing here has run on physical
hardware yet, and there is no Intel VT-x backend:
[docs/roadmap.md](docs/roadmap.md) lists what is missing and in what order
it should arrive.

## Licence

Public domain under [the Unlicense](LICENSE): no rights reserved. This is not CC0.

## Try it without installing anything on real hardware

DOSH runs on a virtual machine. QEMU, the UEFI firmware and every build tool
live in a container, so you don't install them on your system and you don't
need a spare computer. Everything DOSH writes stays inside this checkout,
plus one container image.

### What you need

- `git`
- `podman` (rootless is fine)
- About 3 GB of disk for the container image and build caches.
- Optional, for guests to run on the real processor: an AMD x86-64 host
  with SVM and nested virtualization enabled, and `/dev/kvm` readable and
  writable by you. Without it DOSH runs on QEMU's software processor
  instead. Everything still works, and tests that need KVM are reported as
  skipped with the reason.

This has been built and tested on Linux with an AMD host. Other hosts are
untested.

### For users: run a DOS program in five minutes

```sh
git clone <this repository> dosh && cd dosh
./lab make guests                     # first run builds the lab image; allow a few minutes
./lab tools/doshlab.py HELLO.COM </dev/null
```

Expected result:

```
--- capsule 0 console ---
Hello from DOS!

--- supervisor log (kvm, 0.7s) ---
boot stage=firmware version=0.1.0
boot stage=supervisor capsules=1
hv backend=svm status=enabled
capsule id=0 event=create program=HELLO.COM console=com2
capsule id=0 event=start entry=1005:0100
capsule id=0 event=exit code=0 vmexits=2 preempted=0 stray_io=0
shutdown capsules=1 exited=1 faulted=0
```

The first line of the log says which path ran: `kvm` means guests executed
on your processor, and `tcg` means the software processor.

Other things to try:

```sh
./lab tools/doshlab.py "ARGS.COM one two" HELLOMZ.EXE   # two capsules, a .COM and a .EXE
./lab tools/doshlab.py --attach UPPER.COM               # a filter; type lines, then Ctrl-Z and Enter
./lab tools/doshlab.py --attach ASK.COM                 # sit at a capsule's console (needs a terminal)
./lab tools/doshlab.py --accel tcg HELLO.COM </dev/null # force the software processor
```

Run `./lab tools/doshlab.py --help` for every option. The corpus of programs
is in [guests/](guests/) and what each one proves is in the
[acceptance tests](tests/test_acceptance.py).

#### Running your own program

After `./lab make guests`, copy a `.COM` or `.EXE` into `build/guests/` and
name it on the command line. Only programs in that directory can be run from
the command line. For anything else, call `run()` from `tools/doshlab.py`
in Python. Its `files=` argument puts extra files on drive C:.

What DOSH can run is limited to the [DOS contract](docs/dos-contract.md).
If a program needs something outside it, the log says which call:

```
capsule id=0 event=fault ... reason="unimplemented service INT 21h AH=59h AL=02h"
```

### Notes on running it

- **Standard input.** When the runner isn't attached to a terminal, it
  reads all of standard input before it starts, so always redirect it:
  `</dev/null` for a run with no input. Piped input goes to the first capsule
  once its session opens.
- **The first run is slow.** Building the lab image takes several minutes.
  Later runs start in seconds.
- **No disk changes.** A run's boot disk is a temporary directory, deleted
  when the run ends. `--keep DIR` keeps it for inspection.

### For developers: the full gate

```sh
./lab                                  # a shell inside the lab
./lab make test                        # build, fmt, clippy, unit tests, all acceptance tests
./lab make check                       # just the static checks and the DOS unit tests
./lab python3 -m pytest tests -k NAME  # one acceptance test
```

Read [AGENTS.md](AGENTS.md) before you change anything. It covers how work is
structured here, and [docs/lab.md](docs/lab.md) explains how the lab and the
two execution paths work.

### Cleaning up

```sh
./lab make clean          # removes target/ and build/ in this checkout
rm -rf .lab               # removes the lab's caches
podman rmi localhost/dosh-lab:<tag>   # optional: removes the lab image
```

`podman images | grep dosh-lab` lists the tags to remove.

Without KVM the software processor emulates SVM. The supervisor code is the
same, but nothing is proven about real hardware. Tests run on both paths and
say which one they used.


## Layout

| Path | What |
| --- | --- |
| `crates/supervisor` | The EFI application: boot, platform, hardware virtualization, capsules, presentation |
| `crates/dos` | DOS compatibility services, free of any hardware knowledge and unit-tested on the host |
| `guests/` | The guest corpus: small DOS programs, one per behaviour |
| `tests/` | Acceptance tests: boot the real artifact, run the corpus, check the results |
| `tools/doshlab.py` | Boots DOSH in QEMU and reports what happened; a library and a command |
| `tools/lab/`, `lab` | The development laboratory |
| `docs/` | [Architecture](docs/architecture.md), [DOS contract](docs/dos-contract.md), [lab](docs/lab.md), [roadmap](docs/roadmap.md) |
