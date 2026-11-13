; LOOP.COM - never ends. Interrupts stay enabled; only the supervisor can stop it.
        org 0x100
forever: jmp forever
