# OpenHarmony PDFium prebuilt

The preferred source is a pinned build from
[`OpenHarmonyPCDeveloper/pdfium`](https://gitcode.com/OpenHarmonyPCDeveloper/pdfium):

```sh
. platform/ohos/scripts/env.sh
platform/ohos/scripts/build-pdfium-source.sh
```

`build-pdfium-source.sh` pins the source revision
`f2a7573b5fd1071f61388aeb642329006ae22153`, builds the `pdfium` target for
`target_os="ohos"`/`target_cpu="arm64"`, links the local OHOS SDK into the
source tree's `ohos_sdk/18` path, and stages a stripped
`prebuilt/arm64/libpdfium.so` plus the source `LICENSE`.

The legacy `fetch-pdfium.sh` remains available for offline/bootstrap use. It
downloads the aarch64 `ohos_pdfium` release 7947 prebuilt:

- source: `https://gitcode.com/OpenHarmonyPCDeveloper/ohos_pdfium`
- release: `7947` (PDFium branch, matching pypdfium2 5.12.1)
- archive SHA-256:
  `85126a7d9ddd1be6b5cc8ff84e9027777f36345ce8dcae1c6cd58262d6ae4fd5`
- license: BSD-3-Clause, retained in
  [`LICENSES/PDFium-BSD-3-Clause.txt`](../../../LICENSES/PDFium-BSD-3-Clause.txt)

The archive and source tree are not committed. `build-hap.sh` copies the
library into the hap, strips the staged copy for release builds, and packages
`THIRD_PARTY_NOTICES.md` plus the repository license texts under
`resources/rawfile/` in the hap.

The source build requires a host that can run Chromium's GN/Ninja tools
(normally Linux x86_64) and network access for the first source clone. The
current OpenHarmony aarch64 host cannot execute the downloaded linux64 `gn`
binary; the script should run on the project's Linux build/CI host.

