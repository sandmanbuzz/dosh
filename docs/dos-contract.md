# The DOS compatibility contract

This is what a program running in a DOSH capsule can rely on today. It is
the first rung of the compatibility ladder — the "360K" class: conventional
`.COM` and `.EXE` programs that take arguments, read and write files, talk
to a line-oriented console, and exit.

## The rule

**A service DOSH does not implement is never faked.** A call outside this
contract stops the capsule with a fault that names it:

```
capsule id=0 event=fault at=1005:0757 reason="unimplemented service INT 21h AH=59h AL=02h"
```

That is how the contract grows: run a real program, read what it asked for,
implement that, add the program (or a small one like it) to the corpus.
The line above is real; it came from the first C program tried, and
function 59h was implemented because of it. A list of supported interrupts
is not the goal. Programs that work are.

Each item below is exercised by a unit test in `crates/dos/src/tests.rs`, a
guest in `guests/` with an acceptance test, or both.

## The machine a program finds

| Region | Contents |
| --- | --- |
| `0000:0000` | Interrupt vector table. All 256 vectors are valid and point at service gates. |
| `0040:0000` | BIOS data area: equipment word, 640 KB conventional memory, 80×25 colour text mode. |
| `1000:0000`–`9FFF:000F` | The DOS memory arena, a standard MCB chain. Programs load above 64 KiB. |
| `A000:0000`–`B7FF:000F` | Plain memory. No graphics adapter is behind it. |
| `B800:0000` | The 80×25 colour text buffer (see [The screen](#the-screen)). Plain memory around it. |
| `C000:0000`–`EFFF:000F` | Plain memory. |
| `F000:0000` | Service gates (see [architecture.md](architecture.md)). |
| above `FFFF:000F` | Wraps to `0000:0000`: the A20 line is off. |

The processor is whatever the machine has, in real mode. 32-bit registers
and instructions are available. Port I/O reaches no devices except the
display status port 3DAh (see [The screen](#the-screen)): other writes are
discarded and other reads return all ones. Control registers, MSRs and debug
registers are off limits, and touching them faults the capsule.

The only hardware interrupt delivered is the clock tick, `INT 08h`. There is
no keyboard interrupt: keys are read through `INT 16h` and DOS. The BIOS clock is kept current in the
BIOS data area (0040:006Ch, the tick count, and 0040:0070h, the midnight
flag), and `INT 1Ah AH=00h` reads it. The BIOS clock advances at its own
rate, about 18.2 a second of elapsed time, and each advance is delivered as
`INT 08h` to any program with interrupts enabled, through the IVT. The
supervisor's scheduling quantum is not visible to the guest. A `HLT` outside
a service gate sleeps the program until the next tick, and faults if
interrupts are disabled, because nothing would ever wake it.

## Program loading

- The program is a file on drive C:, loaded by content: an `MZ` (or `ZM`)
  signature means an EXE, anything else a COM.
- **COM**: one segment, image at offset 100h, all free memory allocated to
  it, `SS:SP` at the top of the segment with a zero word pushed so that a
  near `RET` terminates the program. Images over 65,024 bytes are refused.
- **MZ**: header parsed, load module copied, relocations applied, memory
  sized from the header's minimum and maximum allocation, `CS:IP` and
  `SS:SP` set from the header. Data past the load module (overlays, debug
  information) is ignored. Inconsistent headers are refused.
- **PSP**: `INT 20h` at 00h, top of memory at 02h, the CP/M call stub at
  05h, saved vectors 22h–24h, parent PSP (the starting process; the root process is its own parent), a 20-entry job file table, the environment segment at 2Ch,
  `INT 21h / RETF` at 50h, blank FCBs, and the command tail at 80h.
- **Environment**: `PATH=C:\`, `COMSPEC=C:\COMMAND.COM`, then the
  program's full path, as DOS 3 and later provide.
- **Registers at entry**: `DS = ES = PSP`, `AX = 0`, interrupts enabled.

The command tail is passed exactly as written after the program name in the
boot manifest, leading space included, up to 126 characters.

## INT 21h

Errors are reported as DOS does: carry set and the error code in `AX`.

| AH | Function | Notes |
| --- | --- | --- |
| 00h | Terminate | Return code 0. |
| 01h | Read character with echo | Blocks until input arrives. |
| 02h | Write character | |
| 06h | Direct console I/O | `DL = FFh` reads without waiting, reporting through ZF. |
| 07h, 08h | Read character without echo | No Ctrl-C checking on either. |
| 09h | Write `$`-terminated string | |
| 0Ah | Buffered line input | Line editing: backspace, bell when full. Echoes CR on Enter. |
| 0Bh | Input status | |
| 0Dh, 0Eh | Disk reset, select disk | Nothing to flush; C: is the only drive. |
| 0Fh, 10h, 16h, 13h, 17h | FCB open, close, create, delete, rename | Wildcards in delete and rename. |
| 11h, 12h | FCB find first / next | Standard and extended FCBs. |
| 14h, 15h, 21h, 22h, 24h, 27h, 28h | FCB sequential, random and block I/O | State lives in the FCB. The random record is three bytes for records of 64 bytes or more. |
| 19h | Current drive | Always C:. |
| 1Ah, 2Fh | Set / get disk transfer address | Find first/next keep their search state in it. |
| 25h, 35h | Set / get interrupt vector | Act on the real vector table. |
| 29h | Parse filename into an FCB | All four AL flags; wildcards set AL=1; drives other than C: are invalid (AL=FFh). |
| 2Ah, 2Ch | Get date, get time | From the firmware's clock as it read when the supervisor started, advanced by elapsed time; 1980-01-01 00:00:00 if the firmware has none. Hundredths are the supervisor tick. Set date and set time (2Bh, 2Dh) fault by name. |
| 30h | DOS version | 5.00. |
| 39h, 3Ah | Make, remove directory | Access denied: there are no subdirectories. |
| 36h | Get free disk space | Drive C: (DL=0 or 3). Clusters of 8 sectors of 512 bytes, counted from the volume's room; another drive answers AX=FFFFh. |
| 3Bh, 47h | Change, get current directory | The root is the only directory: changing to it succeeds, anything else is path not found; the current directory is the empty string. |
| 3Ch | Create file | Truncates an existing file. Attributes are ignored. |
| 3Dh | Open file | Access mode honoured; sharing bits ignored. |
| 3Eh | Close | |
| 3Fh | Read | From the console: a whole edited line, echoed, CR LF appended; a line starting with Ctrl-Z reads as end of file. |
| 40h | Write | Zero bytes sets the file size. A full disk is a short write. |
| 41h | Delete | |
| 42h | Seek | From start (unsigned), current or end (signed). |
| 44h | IOCTL | 00h device information; 06h, 07h input and output status (a file is input-ready until its end). |
| 45h, 46h | Duplicate handle, force duplicate | Duplicates share one open file, so one file position. |
| 48h, 49h, 4Ah | Allocate, free, resize memory | First fit; free blocks coalesce. A failed grow leaves the block at its largest possible size, as DOS does. |
| 4Ch | Exit with return code | Ends the program. A child's memory and handles are released and its parent resumes; the root program ends the capsule and its code is reported in the supervisor log. |
| 43h | File attributes | Get reports archive (20h) for every file. Set accepts only that: nothing enforces read-only or hidden, so claiming it would be a lie. |
| 4Bh | EXEC, AL=00h | Load and run a program as a child; the caller resumes with carry clear when it ends. The parameter block's environment (0 inherits), command tail and FCBs are honoured. The child gets the largest free block, so a program that wants to start one must give memory back (4Ah) first, as under DOS; otherwise error 8. The child shares the caller's open files (one file position per open file). AL=01h, 03h and 05h fault by name. |
| 4Dh | Child's return code | AL is the code, AH 0 (normal termination); read once. |
| 4Eh, 4Fh | Find first, find next | Wildcard patterns on the root directory; search attributes are ignored because every file is an ordinary one. Failure is error 2 on the first, 12h on the next. |
| 51h, 62h | Get PSP | |
| 56h | Rename | Within the root directory. Refuses wildcards, devices and an existing target. |
| 57h | File time and date | Get and set on an open file. A file nobody has stamped reads 00:00:00 on 1980-01-01: the machine has no calendar. |
| 59h | Extended error | Exact code; conventional class, action and locus. |

## Other interrupts

| Interrupt | Function |
| --- | --- |
| `INT 10h` | `AH=0Eh` teletype output to the console. Once a program calls any other function it has a screen: 00h set mode (2 and 3 only), 01h–03h cursor shape and position, 05h (page 0), 06h/07h scroll, 08h–0Ah read/write character, 0Fh current mode, 13h write string, and the EGA probe (12h, BL=10h) answering "no EGA". One page; other modes and pages fault by name. |
| `INT 16h` | 00h/10h read key (waits), 01h/11h peek, 02h/12h shift state (always 0). Keys are scan code and ASCII; arrows, function keys, Home/End/PgUp/PgDn/Ins/Del decode from terminal escape sequences. No modifiers, no key releases. |
| `INT 11h`, `INT 12h` | Equipment word; conventional memory size. |
| `INT 20h` | Terminate. |
| `INT 29h` | Fast console output. |
| `INT 00h`–`07h` | Processor exceptions. Unless the program has installed its own handler, the capsule faults. |
| `INT 08h` | Clock tick. Its default service does nothing and resumes; the supervisor keeps the BIOS clock, and the gate's stub then calls `INT 1Ch`. Not delivered while a call waits for console input: the tick is held and delivered once the call completes. |
| `INT 1Ch` | User timer hook. Default does nothing. |

## Drive C:

- One flat root directory, held in memory, private to the capsule. Every
  capsule starts with its own copy of the provisioned files.
- Names are case-insensitive. `C:` and a leading `\` are accepted; any
  other drive or any subdirectory is "path not found".
- Names are 8.3, as the disk holds them. A longer name or extension is cut
  (`LONGFILENAME.TEXT` is `LONGFILE.TEX`), as DOS does. A space, a wildcard
  (outside find and rename) or any of `"+,/:;<=>[]|` is "path not found". Two
  long names that cut to the same 8.3 name are one file, as under DOS.
- `CON` and `NUL` are devices, with or without an extension. Standard
  handles 0–2 are the console; 3 and 4 (`AUX`, `PRN`) are null devices.
- Twenty handles per process.
- 4 MiB of space beyond the provisioned files.
- Changes persist only if a disk labelled `DOSHDATA` is attached: when the
  program exits, files it created or changed are written to the disk and
  files it deleted are removed from it. A program that faults, or is
  killed, leaves the disk as it found it. A base-drive file a program
  deletes stays deleted: the disk records it in `WHITEOUT.DSH`, a name
  programs can neither read nor write. A file is refused, and logged
  (`storage event=error ... reason="changed by someone else since
  launch"`), when another capsule changed it after this one launched. A
  program can write no more than the disk has room for. The boot volume is
  never written.

## The console

One character device per capsule, a byte stream in each direction. Until a
program takes over the display (see [The screen](#the-screen)), DOS
output is passed through untranslated: a program that writes CR LF sends
CR LF. On input, CR is Enter, backspace and DEL erase, and a LF directly
from the stream is ignored so that peers which send CR LF work. There is no
type-ahead flush and no Ctrl-C or Ctrl-Break handling.

## Known gaps a real program will hit soon

In rough order of expected demand: EXEC overlays and load-only (4Bh AL=03h,
01h) and the child's own `INT 22h` terminate address; BIOS time of day
(`INT 1Ah AH=02h`, `04h`); text attributes beyond blink/intensity, video
pages; Ctrl-C and critical-error handling (`INT 23h`, `INT 24h`); country
information (38h); terminate-and-stay-resident (31h). The PSP's job file
table is not kept in step with the handles a process holds; no program has
read it yet.

## The screen

An 80x25 colour text buffer sits at B800:0000, and programs may write it
directly. The first `INT 10h` call other than teletype marks the program as
having a screen; from then on its DOS console output is drawn there and the
serial endpoint carries an ANSI rendering (code page 437 as UTF-8, CGA
colours mapped, changes only) instead of the byte stream. Port 3DAh answers
with a synthetic retrace pattern so programs that wait for it proceed.
There is no other adapter I/O: writes to other ports are discarded.

When a program exits, the supervisor logs `file capsule=N name=... size=...
fnv1a=...` for each file it created or changed, since drive C: is not
written back.
