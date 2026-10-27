# OpenHarmony PDFium prebuilt

`scripts/fetch-pdfium.sh` downloads the aarch64 OpenHarmony build of PDFium
used by the PDF preview path:

- source: `https://gitcode.com/OpenHarmonyPCDeveloper/ohos_pdfium`
- release: `7947` (PDFium branch, matching pypdfium2 5.12.1)
- archive SHA-256:
  `85126a7d9ddd1be6b5cc8ff84e9027777f36345ce8dcae1c6cd58262d6ae4fd5`
- license: BSD-3-Clause, retained in
  [`LICENSES/PDFium-BSD-3-Clause.txt`](../../../LICENSES/PDFium-BSD-3-Clause.txt)

The archive is not committed. `build-hap.sh` copies the library into the hap,
strips the staged copy for release builds, and packages
`THIRD_PARTY_NOTICES.md` plus the repository license texts under
`resources/rawfile/` in the hap.

A future revision should replace this platform port with a reproducible build
from a pinned Chromium/PDFium revision, as the other desktop and mobile
packages do with `bblanchon/pdfium-binaries`. The OpenHarmony prebuilt is kept
because it is the currently tested arm64/HarmonyOS-compatible binary.
