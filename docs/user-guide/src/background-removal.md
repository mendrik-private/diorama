# Remove or move a background subject

Background removal is a local cutout workflow, not the **Transparency
background** viewing preference. The preference changes only how transparent
pixels look on screen. This tool changes the document after you activate the
prepared selection.

Before starting, complete the BiRefNet and LaMa parts of [Set up local
models](model-setup.md). BiRefNet is required to make the foreground cutout;
LaMa provides content-aware repair of the background behind it, with a detected
background-colour fallback when LaMa is unavailable.

1. Press <kbd>C</kbd> and draw a tight rectangle around the subject, such as a
   character face and wings.
2. Wait while Diorama prepares the cutout and a replacement background. The
   processing uses the selected rectangle, so include a little surrounding
   background but avoid unrelated subjects.
3. Click **Remove Background from Selected Region** on the selection toolbar.
   If preparation is still running, Diorama reports that it is preparing the
   cutout and applies the request when ready.
4. Drag the cutout to move it. Drag a handle to resize it, or drag just outside
   a corner to rotate it. Diorama keeps the cutout as an image object over a
   repaired copy of the original image. It uses content-aware fill for that
   repaired background when available; if LaMa fill fails, it uses the detected
   background colour and tells you about the fallback.

The cutout is an editable image object. Selecting it later exposes the same
move, resize, and rotate handles. It is distinct from **Flip Horizontally** or
**Flip Vertically**, which mirror the entire image.

[![A prepared character cutout moved over its repaired background with object handles visible](assets/screenshots/background-removal-cutout.png)](assets/screenshots/background-removal-cutout.png)

*The selected foreground remains an editable object over the repaired source
background.*

## Fill versus remove

**Fill Selected Region with Background** immediately paints the full selected
rectangle with a detected background colour, or transparent black when no
consistent border background is found. It is useful to erase a simple
rectangular area, but it does not create a movable foreground and it does not
run subject segmentation. **Remove Background from Selected Region** segments
the visible subject, creates a movable foreground object, and repairs the
underlying selected area of the source image.

If there is no visible content, preparation cannot make a cutout. BiRefNet
segmentation is required: a missing model or a segmentation failure prevents
activation. A content-aware-fill warning is different: the cutout is available,
and only its repaired background falls back to the detected colour. Undo the
completed operation with <kbd>Ctrl</kbd>+<kbd>Z</kbd>; save or export only after
checking the result.

## Clipboard images

Choose **File → Copy Image or Selection** (<kbd>Ctrl</kbd>+<kbd>C</kbd>) to
copy the whole current raster or the active rectangle. Choose **Tools → Paste
Image** (<kbd>Ctrl</kbd>+<kbd>V</kbd>) to add a clipboard image as another
editable image object at the visible center. Move, resize, and rotate it with
the same handles. Pasted images use the **Pasted image scaling** preference
when resizing. Very large clipboard images can be rejected rather than copied.

When an image object such as a cutout is selected, **Copy Image or Selection**
copies that object's isolated, transformed raster, including its transparent
pixels. This is the way to hand a transparent cutout to another application.
