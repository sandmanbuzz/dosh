# Corpus catalogue

The corpus lives in `corpus/` (git-ignored, never redistributed). Tests that
need it skip when a file is absent. This file records what is there so a
result can be reproduced from hashes alone. Folders are grouped by the era of
the DOS services a program demands: `earliest`, `early`, `mid`.

Launch commands run from the repository root. `--corpus DIR` puts every file
in DIR on drive C:, so overlays and message files are found.

Where "Original distribution" says *not recorded*, the file was supplied
locally and its source was not noted; do not guess it.

## Turbo Pascal 2.00B (earliest)

```
Application:        Turbo Pascal
Exact version:      2.00B (banner "Version 2.00B", Copyright 1983,1984 Borland)
Executable:         corpus/earliest/TURBO.COM (36480 bytes); companion TURBO.MSG (1536 bytes)
SHA-256:            232f3aa315a6bd2398ec1ab3156a2c8def5e03a935c9c3123e7e2a763782a9c9 (TURBO.COM)
                    fa28165089320cb8b76f42db03fb85d17da2b3abe099f9a255fb052037c81742 (TURBO.MSG)
Original distribution: not recorded
Known requirements: TURBO.MSG alongside; text console (INT 10h / INT 21h)
Launch command:     ./lab python3 tools/doshlab.py --corpus corpus/earliest TURBO.COM
Initial compatibility observations: Runs to its menu with no fault. Not
                    driven further; the 3.01A build is the one exercised end to end.
Reproducible test scenario: test_full_screen_programs_put_up_their_first_screen[...TURBO.COM-earliest]
Expected result:    Capsule still alive after 10 s, no fault, screen contains "TURBO Pascal system"
```

## Turbo Pascal 2.00B, 8087 build (earliest)

```
Application:        Turbo Pascal (8087 variant)
Exact version:      2.00B (same banner as above)
Executable:         corpus/earliest/TURBO-87.COM (35456 bytes); shares TURBO.MSG
SHA-256:            c755d25c4dad53a5465753b1a03469de7a4d135ea03be162672b747eddfb576f
Original distribution: not recorded
Known requirements: TURBO.MSG; the variant is meant for an 8087 coprocessor
Launch command:     ./lab python3 tools/doshlab.py --corpus corpus/earliest TURBO-87.COM
Initial compatibility observations: not exercised by any test or run
Reproducible test scenario: none yet
Expected result:    none recorded
```

## Turbo Pascal 3.01A (early)

```
Application:        Turbo Pascal
Exact version:      3.01A (Copyright 1983,84,85 Borland)
Executable:         corpus/early/TURBO.COM (39671 bytes); companion TURBO.MSG (1536 bytes)
SHA-256:            e91124755d428dab5c75862fac40c34bb2e96c7864cb68834df7d3f45bc87e86 (TURBO.COM)
                    fa28165089320cb8b76f42db03fb85d17da2b3abe099f9a255fb052037c81742 (TURBO.MSG, identical to the earliest copy)
Original distribution: not recorded
Known requirements: DOS version check (message "Incorrect DOS version"); TURBO.MSG; enough free memory
Launch command:     ./lab python3 tools/doshlab.py --corpus corpus/early TURBO.COM
Initial compatibility observations: Edits, compiles to a .COM and runs a
                    program inside the capsule. A keypress during compilation
                    aborts it, so scripted input must wait for output. Quit
                    asks "Save (Y/N)?".
Reproducible test scenario: test_turbo_pascal_compiles_and_runs_a_program_started_from_the_shell
                    and test_a_program_built_by_a_compiler_in_the_capsule_runs_in_the_capsule
Expected result:    The compiled program's output appears on the console and the
                    built .COM runs from COMMAND.COM with exit code 0
```

## WordStar 3.31q (early)

```
Application:        WordStar
Exact version:      Release 3.31q, ID # 213442Q4-001 (Copyright 1979, 1985 MicroPro)
Executable:         corpus/early/WS.COM (21376 bytes)
SHA-256:            5129c93388c5505fd673ad585a998de5f814d0ec388cc55d21454e8d261ec5d2
                    17170e40570eec20bc7222fd9e7f95b7cde8a1f0e64e13f2e5222a46009ce725 (WSMSGS.OVR, 29056 bytes)
                    c36fb19139d8dc894e6efaa892e61f7349aaa445d27883a995a9a722f6b61595 (WSOVLY1.OVR, 41216 bytes)
Original distribution: not recorded
Known requirements: WSMSGS.OVR and WSOVLY1.OVR on the same drive; full-screen text console, keyboard
Launch command:     ./lab python3 tools/doshlab.py --corpus corpus/early WS.COM
Initial compatibility observations: Opening menu draws; documents can be
                    typed and saved to C:.
Reproducible test scenario: test_full_screen_programs_put_up_their_first_screen[...WS.COM-early]
                    and test_wordstar_saves_a_document
Expected result:    Screen shows "O P E N I N G   M E N U"; after typing and saving,
                    the file exists on C: with the typed text
```

## ARC 5.20 (early)

```
Application:        ARC archive utility (System Enhancement Associates)
Exact version:      5.20, created 10/24/86 14:56:41
Executable:         corpus/early/ARC.EXE (30142 bytes)
SHA-256:            de6a69d65b6946589fe0522fada788c3e9c09c301ab7fde63b9f596a9a0b0489
Original distribution: not recorded
Known requirements: DOS 2.0 or later (its own message "NEEDS DOS 2.0")
Launch command:     ./lab python3 tools/doshlab.py --corpus corpus/early ARC.EXE l T.ARC
                    (needs an archive named T.ARC on the volume; the tests build one)
Initial compatibility observations: list, print, extract and add all work;
                    a corrupted member is reported with a nonzero exit.
Reproducible test scenario: the ARC tests in tests/test_acceptance.py (list, p, x, bad CRC, a)
Expected result:    Listing shows the member; `p` prints its text; `x` writes it
                    with a matching CRC; a damaged archive exits nonzero; `a`
                    creates a T.ARC larger than zero and smaller than a third of a
                    repetitive input
```

## VisiCalc Advanced Version (earliest)

```
Application:        VisiCalc Advanced Version
Exact version:      banner "VisiCalc Advanced Version", Copyright 1979, 1984 Software Arts; no further number found
Executable:         corpus/earliest/VC.COM (27639 bytes)
SHA-256:            f1ee59432f7cdaa03cde529f7c5cdec1aba659440044b3a7ce7923b7dc801e2f
                    c11f7b85264a193fe586a8234c889b1c14a9d789cd7db43a14e7cf565f433c88 (VC.OVL, 144352 bytes)
Original distribution: not recorded
Known requirements: VC.OVL alongside; full-screen text console
Launch command:     ./lab python3 tools/doshlab.py --corpus corpus/earliest VC.COM
Initial compatibility observations: First screen draws. Spreadsheet
                    interaction is not yet driven.
Reproducible test scenario: test_full_screen_programs_put_up_their_first_screen[...VC.COM-earliest]
Expected result:    Waiting for a key after 10 s, no fault, screen contains "VisiCalc Advanced Version"
```

## TheDraw (mid)

```
Application:        TheDraw (text-mode art editor)
Exact version:      not identified: strings show no release number. Messages
                    mention a registered version and "TheDraw v0.00 or higher"
                    (file-format check), so treat the version as unknown.
Executable:         corpus/mid/THEDRAW.EXE (151008 bytes, MZ executable)
SHA-256:            39d030c9430c7cab0e740831352e06d47c355e4b485567fabb206e01a36fa292
Original distribution: not recorded
Known requirements: Swap file SWAPFILE.TD; COMSPEC/COMMAND.COM for its DOS
                    shell; font files (THEDRAW.PCK, fonts); mouse optional;
                    EGA/VGA video BIOS
Launch command:     ./lab python3 tools/doshlab.py --corpus corpus/mid THEDRAW.EXE
Initial compatibility observations: Faults immediately after start with
                    vmexits=27: "unimplemented service INT 10h AH=11h AL=30h"
                    (character generator, get font information). Fault is by
                    name as the contract requires; nothing is faked.
Reproducible test scenario: run the launch command above (not yet an acceptance test)
Expected result now: capsule fault with the reason above. Target: draws its
                    editor screen and waits for a key with no fault, so it can
                    join test_full_screen_programs_put_up_their_first_screen
```
