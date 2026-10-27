# Continuous Block and Chat canvas

Block and Chat use a continuous pixel viewport over the existing, bounded
terminal transcript. Each shell session owns its viewport and pending motion;
switching tabs or splitting a tab does not transfer another pane's momentum.

## Interaction

- Precision touchpad deltas are applied directly, including their fractional
  pixels. The platform's momentum is not passed through a second inertial
  integrator. Discrete wheel steps use the shared, elapsed-time
  `jterm_core::transcript_canvas::CanvasMotion` spring.
- The final fractional position is retained when scrolling stops. Crossing a
  text row does not snap the document or reset its position.
- One real adjacent history row is materialized while between rows. Text,
  selection highlights, command cards, headers, links, and the scrollbar use
  the same presentation coordinates. The PTY retains its original dimensions.
- Chat's shell composer stays at the same screen position. History and the
  fixed composer have separate clips in both the GPU and CPU render paths;
  scrolling history cannot paint through the input surface.
- Short transcripts, including collapsed Block histories, start at the top
  of the canvas and grow downwards. In Chat, spare space sits between history
  and the composer. Docking moves the real input rows once, preserving their
  cell provenance without a second copy.
- A running command reserves its metadata row from the start, so completion
  changes its status without moving the output. Separately delivered shell
  completion and prompt markers retain the same canvas between PTY reads.
- Following live output gently approaches the new tail. Large bursts limit
  visual catch-up to two viewport heights instead of racing through thousands
  of intermediate rows. This intentionally jumps ahead across the excess burst
  before easing to live; all output remains available in the transcript.
  Browsing history
  retains its stable raw-row anchor, including the fractional offset, when
  output arrives, history is trimmed, or a command block changes height.
- Resizing a pane or changing font size keeps a reader's position within the
  anchored row, rather than snapping to a text-row boundary. A layout change
  retires obsolete wheel motion; a live-following pane stays at the live edge.
- Explicit navigation and scrollbar dragging replace the old destination.
  Selecting history stops pending motion and holds the reading position as
  more output arrives; typing or returning to live restores following. A
  resumed tab settles an old animation instead of replaying stale momentum.
  Settled canvases do not schedule animation frames; system reduced-motion
  preference disables interpolation while retaining precise coordinates.

## Rendering and scope

Completed successful Block history shares the underlying canvas instead of
painting a permanent card fill and border. Hover, selection, failures,
background jobs and live input keep their existing semantic emphasis, but
without floating shadows. These are paint-only changes: text insets, row
heights, pointer regions and Chat's command/composer silhouettes are unchanged.

The existing cell/version cache remains the source of text. Fractional frames
update GPU uniforms and native chrome geometry; they do not reconstruct the
scrollback. CPU fallback uses the same fractional transform. Both standard
and dual-source GPU glyph shaders clip independently at the composer edge.

This is a presentation layer, not a new shell editor or unbounded history
allocation. Alternate-screen applications, native Vim, and sessions showing
Kitty graphics retain their existing compatibility/native scrolling. No
synthetic text, delayed terminal bytes, or decorative per-character typing
animation is introduced. The window still owns one ASCII companion.

## Verification

`cargo test --locked --lib transcript_canvas` covers fractional resting
positions, continuous output following, anchored reading, shrinking and
fixed-capacity history, resumed tabs, the actual projected margin row, fixed
composer and pointer correspondence, short transcript growth, unique input
provenance, selection ownership, renderer reuse across sessions, and validation
of both WGSL shader variants. Existing Chat and Block interaction tests remain
part of the renderer regression checks.

Native window screenshots and software-rendered wheel traces validate visible
intermediate positions separately. These checks do not establish physical-GPU
frame latency or high-refresh touchpad behavior on a user's Wayland desktop.
