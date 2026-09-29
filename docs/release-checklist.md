# Release documentation checklist

Complete this checklist before creating or pushing a release tag. It implements
[DEC-0001](decisions/0001-release-finalization-includes-the-visual-user-guide-and-github-pages-documentation.md).

1. Identify the previous release tag and review every change since it. Walk the
   [user-facing capability coverage matrix](documentation-plan.md#user-facing-capability-coverage)
   against the change set; map each affected capability to every affected page
   in `docs/user-guide/`, including feature steps, expected results, disabled
   states, and keyboard shortcuts. Record intentionally unchanged entries in
   the release-preparation record.
2. Update each affected guide so its instructions describe the release build.
   Capture affected features in the real Diorama application; use a copy of an
   image from `/home/mendrik/Pictures/game-assets/characters-midwalk-v2/horror-variants/dnd-fantasy`
   when it helps demonstrate a feature, and preserve the source artwork.
3. Make visual explanations readable. Annotate relevant faces or wings with
   arrows, curved text, and highlight markers where useful. Choose each
   annotation colour for contrast with the artwork beneath it; for example,
   never put red markings on red artwork. Confirm text and markers remain
   legible at the image size used by the guide. Capture at normal display
   density: set `GDK_SCALE=1` and `GDK_DPI_SCALE=1`, and force GTK's Xft DPI to
   96 for the headless test compositor when it inherits a high-DPI desktop
   setting. Review every capture both as the guide thumbnail and at native PNG
   size; reject oversized typography, clipped controls, or unreadable labels.
4. Record screenshot provenance beside the affected guide or its asset: Diorama
   version, source revision, capture date, feature state, and sample-image
   filename. Replace obsolete screenshots rather than presenting old behaviour.
5. Run `sh build-aux/build-docs.sh` to build the documentation and check its
   links and images. Fix every failure before continuing.
6. Push the completed main revision, then verify its successful GitHub Pages
   deployment is for that exact commit. Record the commit and deployment URL in
   the release notes or release-preparation record. Only then create and push
   the release tag.

The tagged release workflow runs the documentation build again and cannot
publish the GitHub Release unless that validation succeeds.
