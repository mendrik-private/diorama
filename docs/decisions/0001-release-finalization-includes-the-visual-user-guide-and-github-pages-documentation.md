# DEC-0001: Release finalization includes the visual user guide and GitHub Pages documentation

Status: accepted

## Rationale

User explicitly requested automatic GitHub Pages docs builds and that Crusty remember to update docs while finalizing a release.

## Applies to

- release
- documentation
- AGENTS.md
- docs/user-guide
- .github/workflows/release.yml
- .github/workflows/docs.yml

## Consequences

- Before preparing or finalizing a release, follow docs/release-checklist.md: compare changes since the previous release with user-guide coverage, update affected steps/shortcuts/screenshots, and run the documentation build and link/image checks before tagging.
- Explain features visually using real Diorama screenshots. User-authorized sample artwork is /home/mendrik/Pictures/game-assets/characters-midwalk-v2/horror-variants/dnd-fantasy; preserve originals. Faces and wings can be annotated with arrows, curved text, and highlight markers; choose contrasting colors, never red on red or similarly unreadable combinations.
- Verify successful GitHub Pages deployment for the released main revision. Report documentation verification and any remaining gaps explicitly.
