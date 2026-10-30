# Get started

## Install

Download `Diorama.flatpak` from the [latest GitHub release](https://github.com/mendrik-private/diorama/releases/latest), then run:

```sh
flatpak install --user ./Diorama.flatpak
flatpak run io.github.mendrik_private.Diorama
```

## Open and browse

1. Choose **File → Open…** or press <kbd>Ctrl</kbd>+<kbd>O</kbd>.
2. Pick an image. Diorama builds a sequence from its folder so you can browse
   nearby supported images.
3. Use the previous and next controls, the left and right arrow keys,
   <kbd>Alt</kbd>+<kbd>Left</kbd>/<kbd>Right</kbd>, or
   <kbd>Page Up</kbd>/<kbd>Page Down</kbd> to move through the sequence.

Diorama opens PNG, JPEG, GIF, WebP, AVIF/HEIF, BMP, TIFF, SVG, JPEG 2000,
JPEG XL, QOI, ICO, EXR, Netpbm, TGA, XBM, and XPM images when their installed
image loader supports them, and WebM videos through the GNOME runtime's video
decoders. Animated files with more than one frame, and WebM videos, show
previous-frame, play/pause, and next-frame controls in the header. Stepping a
frame pauses playback; press **Play animation** to resume. WebM plays muted and
loops; frame stepping in a video moves by roughly one frame.

A paused WebM frame is the image you edit. Choosing a drawing tool, applying an
edit, saving, copying, or printing pauses the video first, so the result always
matches the frame on screen. Video playback stays unavailable while the frame
has edits; undo them to play again. Save a WebM frame with **Save As…** as PNG
or JPEG.

[![Diorama browsing fantasy-character PNG files](assets/screenshots/overview.png)](assets/screenshots/overview.png)

*The character set is a useful way to check the previous and next image
controls. Select the image to view it at full size.*

## Set a comfortable view

Press <kbd>0</kbd> for **Fit** and <kbd>1</kbd> for actual size. Number keys
<kbd>2</kbd> through <kbd>9</kbd> select 200% through 900%; <kbd>+</kbd> and
<kbd>−</kbd> make smaller adjustments. <kbd>Ctrl</kbd>+scroll zooms around the
pointer, and use the middle mouse button to pan a zoomed image. Press <kbd>X</kbd> to toggle
soft and hard display filtering. Hard rendering keeps pixel art crisp; soft
rendering interpolates painted and photographic artwork.

Pinch to zoom on a touchpad or touchscreen. When an image overflows the view,
an overview map appears at the upper left; click or drag it to pan. Diorama
follows the folder's Nautilus sort order when available, otherwise it uses
natural filename order (`image2` before `image10`).

Use **Info → Fullscreen Image Preview** or <kbd>Space</kbd> to inspect the
image without editing controls; press <kbd>Escape</kbd> to return. <kbd>F11</kbd>
toggles the normal application fullscreen mode.

## Files and sequences

**File → Open With…** opens the current file in another application through
the system chooser. At the top of the main menu, **Copy Filepath** copies the
local path. Directly below it, **Open Location** opens the current file in its
parent folder in the desktop file manager (Nautilus on GNOME). These actions
are unavailable until an image with a local file is open; **Open Location**
also requires a parent folder.

[![Main menu with Copy Filepath and Open Location above File, Tools, Edit, Transform, and Info](assets/screenshots/main-menu.png)](assets/screenshots/main-menu.png)

*Open the hamburger menu to copy the current path or find the image in its
folder. The existing grouped commands follow these two actions.*

**Print…** opens the system print dialog; it prints the displayed animation
frame for an animation or video and uses white paper regardless of the viewer's
transparency backdrop. Use **Delete Image** (<kbd>Delete</kbd>) only when you
intend to delete the current file; Diorama asks for confirmation.
