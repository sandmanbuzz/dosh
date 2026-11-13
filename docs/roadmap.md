# Roadmap

Where DOSH goes from the first vertical slice, in the order that seems
right today. Each step is a capability with an acceptance criterion in the
form the project prefers: real programs that work.

## Things that could prove awkward

Flagged now rather than discovered later.

- **Intel.** There is no VT-x backend. Real mode under VT-x needs
  "unrestricted guest" (every Intel processor of the last fifteen years
  has it) and EPT. The work is well understood but is a whole second
  backend, and it cannot be tested in the software-processor lab path,
  because QEMU does not emulate VT-x. Intel testing will need KVM on an
  Intel host.
- **Physical hardware.** Everything so far ran under QEMU. Real machines
  will differ in firmware behaviour, in whether legacy UARTs and a PIT
  exist at all, and in what SMM does behind the supervisor's back.
  A machine with no serial port has no console transport today.
- **Work arrives only at boot.** After `ExitBootServices` the supervisor
  has no storage or network driver. "Offer it work" at runtime, remote
  launch, and persistent drives all wait on the first real device driver.
- **Protected mode is a different animal.** See the last step below.

## 1. Time and interrupts for guests

Give capsules a virtual interval timer and interrupt controller, and inject
interrupts into guests (SVM virtual interrupts and event injection). This
unlocks the timer tick (`INT 08h`/`1Ch`), `HLT` as an idle that wakes,
BIOS time (`INT 1Ah`), DOS date and time, and interrupt-driven keyboard
input (`INT 09h`/`16h`). It also lets the scheduler treat a halted guest as
idle.

*Status:* done. Timer tick, `INT 1Ah`, DOS date and time, and `HLT` idle exist; guests get interrupts through the timer gate.

*Done when:* programs that poll the BIOS tick or wait in `HLT` loops run,
and a guest's clock advances at the right rate while other capsules are
busy.

## 2. A supervisor that stays up

Replace run-the-manifest-then-power-off with a supervisor that waits for
work: a management channel (first on the log serial port) to list, launch,
suspend, resume and terminate capsules, plus a watchdog policy for capsules
that run away. Capsule state is already self-contained (`Vm` plus `Dos`),
so suspend and resume are bookkeeping, and snapshot is serialising those
two things.

*Status:* done. With `supervisor stay` in `BOOT.CFG` the supervisor waits on COM1 for `list`, `launch`, `suspend`, `resume`, `kill`, `files`, `put`, `get` and `shutdown`. Suspend freezes the guest's clock. Not done: snapshot to disk, and an authenticated channel.

*Done when:* a test launches a capsule, suspends it mid-computation,
launches and finishes a second one, resumes the first to the right answer,
and kills a third that never exits.

## 3. The screen

Most DOS software addresses the screen. Add the video BIOS text services
(`INT 10h`), a text-mode surface backed by the guest's `B800:0000` memory
(change tracking through nested page tables), and a second kind of
presentation endpoint that renders a surface to a terminal. Keyboard
scancodes arrive with step 1.

This is where the presentation boundary earns its keep: the capsule gains a
display device; the endpoint decides whether it is drawn locally, sent down
a serial line as escape sequences, or shipped over a network.

*Status:* mostly done. `INT 10h` text modes, an 80x25 buffer at `B800:0000`, `INT 16h` keyboard decoding and an ANSI renderer (`crates/term`) exist; Turbo Pascal, WordStar and VisiCalc reach their screens over a serial terminal. Change tracking uses per-tick snapshots, not nested-page-table dirty bits. Switching between capsules redraws afresh through the mux.

*Done when:* a full-screen text application — an editor, a file manager —
is usable through a terminal, and switching the terminal between two such
capsules redraws correctly.

## 4. DOS breadth, driven by a real corpus

Grow the contract the way [dos-contract.md](dos-contract.md) describes,
against third-party binaries rather than programs written for the purpose:
find first/next, directories, attributes and timestamps, `EXEC` and child
processes, FCBs, handle redirection, Ctrl-C and critical errors,
terminate-and-stay-resident.

Two pieces of lab work make this fast:

- a corpus of freely redistributable DOS binaries fetched by hash at test
  time (FreeDOS utilities, Info-ZIP, period compilers and assemblers that
  are legally available);
- differential testing against an established DOS implementation —
  DOSBox is in the lab's distribution — so expected output comes from an
  oracle instead of from reading interrupt lists.

*Status:* in progress. Find first/next, attributes and timestamps, `EXEC`, FCBs and handle duplication exist, driven by the local corpus ([corpus.md](corpus.md)); Turbo Pascal builds a `.COM` inside a capsule. Not done: an unzip utility, directories, Ctrl-C and critical errors, TSRs, and the redistributable corpus and differential testing described below.

*Done when:* an unzip utility extracts an archive, and an assembler
assembles a program that DOSH then runs. That closes the 360K class and
most of 360K+.

## 5. Storage, then network

A first block driver (virtio-blk in the lab; AHCI or NVMe for real
machines) and FAT, so drives persist and work can be loaded after boot.
Then a network driver and a small IP stack, so capsule consoles and
surfaces can be attached remotely and capsules launched over the wire.
This is the "Citrix" part of the idea, and where serial endpoints stop
being the only transport.

*Status:* half done. A polled virtio-blk driver and a FAT driver (`crates/fat`) give drive C: a persistent disk: a program's output file survives a reboot, and the operator can store programs on the disk and launch them after boot. `tools/doshlab.py serve` puts the management and console lines on TCP, so `socat` attaches to a running capsule from another machine; the TCP is QEMU's. Not done: a network driver and IP stack in the supervisor, a driver for real storage hardware (AHCI, NVMe), modern virtio, subdirectories and long names on the disk.

*Done when:* `socat` on another machine attaches to a running capsule over
TCP, and a program's output file survives a reboot. (The second half is
met. The first is met through the lab's virtual machine; with a supervisor
network stack it would be met on bare metal.)

## 6. The rest of the machine room

- **VT-x backend**, behind the existing `hv` surface.
- **All the processors**: start the application processors, per-CPU run
  queues, per-guest address-space IDs.
- **Physical hardware bring-up**, starting with one AMD machine with a
  serial port.
- **Supervisor hardening**: W^X page tables, per-capsule memory budgets,
  filtered `CPUID`.

## 7. Bigger memory: the 720K / 1.2M class

The A20 gate (port 92h and the keyboard controller), the high memory area,
XMS and EMS services provided by the supervisor rather than by drivers
loaded in the guest, upper memory blocks. Guest memory beyond the first
megabyte is a nested-paging change, not a new mechanism.

*Done when:* programs that require XMS or EMS find it and use it.

## 8. Protected mode: late DOS

DOS extenders and DPMI clients are the largest step, and not an extension
of the real-mode work.

The capsule design helps in one important way: a capsule is a
hardware-virtualized machine with its own nested page tables, so a guest
entering protected mode — its own descriptor tables, its own page tables —
needs no emulation at all. Lifting the control-register intercept is most
of what "allow protected mode" means, and extenders that bring their own
ring-0 kernel and ask the environment only for raw memory (`INT 15h`, XMS,
VCPI) may run on little more than step 7.

The hard part is DPMI: a host service with a large interface (descriptor
management, mode switches, interrupt reflection between modes, memory) that
clients expect to be provided for them. It can live in the supervisor,
entered through gates like DOS services are, or run as trusted code inside
the capsule. That choice deserves its own design document, written when
steps 1–7 have shown what real extenders demand.

*Done when:* a DPMI-hosted compiler builds a program, and a well-known
protected-mode game reaches its title screen — which also needs VGA
graphics, the other large piece of the late-DOS class.
