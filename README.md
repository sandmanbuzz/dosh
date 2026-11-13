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

## Try it

The only host requirement is `podman`. Everything else lives in the lab
container, which is built on first use.

```sh
./lab make test                       # build everything, run every test
./lab tools/doshlab.py HELLO.COM      # boot DOSH, run one program, show what happened
./lab tools/doshlab.py "ARGS.COM one two" HELLOMZ.EXE     # two capsules
./lab tools/doshlab.py --attach ASK.COM                   # sit at a capsule's console
```

```
$ ./lab tools/doshlab.py HELLO.COM
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

With `/dev/kvm` and nested virtualization on an AMD host, guests execute on
the physical processor. Without it the lab falls back to QEMU's software
processor, which emulates SVM: the same supervisor code runs, but nothing is
proven about real hardware. Tests run on both paths and say which.
[docs/lab.md](docs/lab.md) has the details.

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
