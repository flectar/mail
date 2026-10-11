# CPU SVG profile comparison

This independent Rust fixture uses the current production search, sync and inbox
SVG assets and the exact resvg **0.48.1** / tiny-skia **0.12.0** raster path. Its
size calculation and transform match Slint **1.18.1** `ParsedSVG::render`.
It does not build the mail application or HAP, access accounts, or use a device.

From the repository root, using the validated Rust **1.97** host toolchain and
Python **3.11+**:

```sh
python3 platform/ohos/tests/run_svg_profile_contracts.py \
  --output-dir target/ohos-host-contracts/svg-profile
```

The output directory must be new or empty. The runner builds only this fixture
with Cargo `--offline --locked`, uses the compiler's host triple explicitly, and
removes its temporary build tree after the run. Dependencies must already be in
the local Cargo cache; a missing dependency causes an offline build error in the
retained build log. It does not download or install an SDK or simulator.

To retain the isolated host target for future runs:

```sh
python3 platform/ohos/tests/run_svg_profile_contracts.py \
  --target-dir /private/tmp/feihong-svg-profile-repro-target \
  --output-dir target/ohos-host-contracts/svg-profile-repro
```

`Cargo.lock` pins every transitive dependency. The runner checks that the eight
packages under comparison also occur at these versions in the production OHOS
lock: bytemuck **1.25.2**, kurbo **0.13.1**, resvg/usvg **0.48.1**, strict-num
**0.1.1**, svgtypes **0.16.1**, tiny-skia/tiny-skia-path **0.12.0**.
Both builds preserve debug assertions and debug information. The harness uses
opt2 in both builds; dependencies use opt0 in `dev`. The `svg-opt3` profile
changes only the eight listed packages to opt3. Verbose build logs retain actual
compiler flags; cached packages are identified as cached rather than reported
as newly observed compiler invocations.

For each of three icons, physical sizes 24, 45, 54 and 144 produce **12 cases**.
The default SVG `currentColor` is black in both profiles. The runner checks exact
premultiplied RGBA equality across four processes (`opt0`, `opt3`, `opt3`,
`opt0`), buffer sizes, nonempty antialias coverage and premultiplied channel
bounds. After eight warmup renders, each process measures seven batches of 32
allocation-and-raster calls and separately seven batches of 32 fresh parses.
Results report the median of all 14 batch averages per profile and case. There
are no timing pass thresholds and no discarded samples. Source hashes, raw
pixels, all timings, logs and source/lock snapshots are retained with file mode
0600 in the evidence directory.

The original host experiment passed all 12 exact-pixel groups, with allocation
and raster medians showing opt0/opt3 ratios of approximately **13–17** on the
development Mac. Its complete record is
`target/ohos-evidence/svg-profile-host/validation.json`; the same directory
contains the exact source, manifest, lock and logs used for that run. The
reusable fixture preserves that raster and measurement code; its production
repository path is now a command-line argument. These records are host evidence
for the cost direction of the profiled SVG path, **not device FPS or HAP
acceptance**. Host load was not controlled. The fixture does not cover SVG text
or raster-image assets, tiny-skia 0.11.4, Slint's downstream image colorization,
the full UI, or OHOS presentation. Those require separate app/device validation.
