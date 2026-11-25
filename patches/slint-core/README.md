# Flings with reduced motion

Slint 1.19 starts no fling when the operating system asks for less motion
([Slint #13630](https://github.com/slint-ui/slint/pull/13630)). On Android it
takes that from `ValueAnimator.areAnimatorsEnabled()`, which Battery Saver and
the animator duration scale in Developer options turn off as well as Remove
animations. Every list in Flectar Mail then stopped dead where the finger left
the screen, while Android's own lists keep moving: `OverScroller` times a fling
with the frame clock and never reads the animation scale. WCAG counts the
movement of scrolling itself as essential, not as animation to remove.

## Sources and adaptation

`crates/i-slint-core` is the published 1.19.0 archive, revision
`e436f546f71481d786cbc2a74316b1be1ef678c1`, plus two patches, which are ours.
`0001-keep-flings-with-reduced-motion.patch` lets a release fling as usual
while motion is reduced. What stays reduced is the decoration around it. Content
dragged past an edge goes back at once instead of springing, and a fling stops
at the edge instead of bouncing. Wheel steps, `scroll-to()` and content that
ends up out of bounds still land at once, as in 1.19. Upstream master has the
same code.

`0002-dependency-scanner.patch` lists three dependencies that the release
declares but does not import, so `cargo machete` can scan the crate.

`Cargo.toml.orig` is kept unchanged for provenance. The standalone lockfile is
omitted, and the application lockfiles resolve the patched crate.

## Verification and removal

```sh
python3 scripts/verify-slint-core.py
```

The verifier downloads the checksum-pinned archive, checks its revision,
applies the patch, and compares every file with the vendored crate. It also
checks that the root, Android and OpenHarmony workspaces patch the crate and
that their lockfiles resolve it locally for Slint 1.19.0.

When a Slint release keeps flings with reduced motion, remove the vendored
crate, this directory, the three manifest patches, the root workspace
exclusion, the `.gitattributes` entry, the verifier, its CI step, and its
paragraph in `THIRD_PARTY_NOTICES.md`. Then update the three lockfiles and
`packaging/flatpak/cargo-sources.json`.
