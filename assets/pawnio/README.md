# PawnIO files bundled in the Windows build

Windows exposes no AMD CPU temperature without a kernel driver. Vitals uses
[PawnIO](https://pawnio.eu), a Microsoft-signed driver that only runs signed,
purpose-built modules, to read the Zen `Tctl` sensor.

| File | Version | Source | License |
|---|---|---|---|
| `PawnIO_setup.exe` | 2.2.0 | [namazso/PawnIO.Setup](https://github.com/namazso/PawnIO.Setup/releases/tag/2.2.0) | Proprietary binary, redistribution allowed by the author |
| `AMDFamily17.bin` | 0.2.11 | [namazso/PawnIO.Modules](https://github.com/namazso/PawnIO.Modules/releases/tag/0.2.11) (source: `AMDFamily17.p`) | LGPL-2.1, see `COPYING-modules` |

Both files are embedded in `vitals.exe`. When Vitals runs as administrator and
PawnIO is missing, it asks once whether to run the installer. It then reads the CPU
temperature whenever it runs as administrator, because the PawnIO device only
accepts administrators.
