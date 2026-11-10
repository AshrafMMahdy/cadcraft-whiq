# The block-expansion limit (`MAX_BLOCK_EXPANSION` = 2,000,000)

Written on branch `4l13-round2b` (from `whiq` at 08af9f2). Part A of the round: what the limit is for, what the
agent sees, what can be counted up front, the options, a recommendation, and what was built now (A4). The limit's
value and behaviour are unchanged.

## 1. What the limit is and what it protects

`Builder::expand()` (crates/render/src/lib.rs) takes one unit for every block copy (each cell of a MINSERT array) and
one for every entity drawn inside a copy; a nested reference is itself an entity and then expands its own copies.
Dimension and multileader blocks count the same way. When the units are spent, `expand()` returns false and the
remaining block contents are not drawn (the build still counts the calls it makes, so `DisplayList::expanded` goes
past the limit; `Options::blocks_truncated(expanded)` is that test). The doc comment on the constant names the reason:
a hostile or broken file with nested, self-referencing or arrayed references expands exponentially
(`MAX_BLOCK_DEPTH` = 16 only bounds the depth: a block holding four references to the next, 15 deep, is 4^15 line
copies; one MINSERT of 10,000 x 10,000 is 100 million copies). It is CWE-400 protection.

What it protects, measured:

* **Time.** A build costs about 0.8 microseconds per expanded entity for line blocks: 1.8 million expansions (20 grids x
  100 rows x 100 trees, each tree a circle and six lines) built in 1.5 s here. Past the limit, without it, a bomb runs
  for minutes to hours.
* **Memory of the display list.** The list holds `DPrim` (32 bytes) per primitive plus 16 bytes per vertex or triangle
  point. Per expanded entity that is **36 bytes** for plain line blocks, **225 bytes** for the tree above (a circle
  tessellated at 0.001), and for whole real drawings in the corpus (model space, non-block geometry included) a median
  of **2.4 KB** and up to **21 KB** per expanded entity (hatches, text, dimensions; 43 drawings with 20 or more
  expansions). So 2,000,000 expansions is anywhere from about 72 MB to several GB. The limit counts entities, not bytes,
  so it is a loose proxy for memory; it is a hard stop for time and for bombs.
* It does **not** protect against nesting depth (that is `MAX_BLOCK_DEPTH`).

## 2. Consumers: what holds the expanded entities, for how long, what the agent sees

| Consumer | Holds | For how long | At the limit |
|---|---|---|---|
| `render_preview` svg (preview.rs `list_for`, `svg_tile`) | the whole display list of the tile, then the whole SVG `String` of the tile (and the JSON answer when it is returned inline) | one tile | `blocks_truncated` (since the last release) |
| `render_preview` png (`preview::png`) | the display list of the tile plus the pixmap | one tile | `blocks_truncated` |
| `export` svg model space (`write_svg_sliced`) | one slice of 16,384 top-level entities (`SLICE_ENTITIES`) as a display list, dropped before the next; the file is written as it goes | one slice (but a slice whose entities expand a lot is up to the whole limit: slicing is by top-level entity, not by expansion) | `blocks_truncated` |
| `export` svg of a layout | one slice at a time since this round (`build_layout_slices`: the sheet's objects and each viewport's contents in draw order); before it the whole layout list | one slice | `blocks_truncated` |
| `export` png | as `render_preview` png | the export | `blocks_truncated` |
| `export` pdf (`io::pdf::plot` -> `sheet`) | one `DisplayList` per sheet (`build_plot` of the whole space; the plot window only places the drawing on the page, it does **not** leave out block references outside it), then the PDF writer's own document in memory, then the `Vec<u8>` | per sheet; the document until the file is written | **nothing before this round** (the answer never said); now `blocks_truncated` too |
| the UI canvas (`ui-egui` `canvas/list.rs`, `rebuild.rs`) | the whole list of the open space, rebuilt at every 2x zoom step and edit, kept while the view is open | while the view is open | silent: no message to the user (a gap, not changed here) |
| engine `plot` preview (`cmd/exchange.rs`), `io::write_framed`, `io::plotter` svg/png | the whole built list, then the output as one `String` / `Vec` | the call | silent |

**Read and query tools do not go through expansion.** `read_drawing` (entities, layers, blocks, layouts, text,
dimensions, takeoff), `search_drawing`, measuring and the layer list read the stored entities; with `expand_blocks`
they walk the block table themselves (`read.rs` `walk`: depth bounded, a block already on the path is not entered
again, no allocation per expanded entity, cancel checked every 4,096 visits). Extents (`doc/extents.rs`) are memoised
per (block, depth). So none of them is affected by `MAX_BLOCK_EXPANSION`; they have their own separate bounds (the
snapping code has `MAX_BLOCK_LEAVES` = 20,000, a different cap not touched here).

**What the agent saw before this round:** `render_preview` and `export` svg/png: `blocks_truncated: true`,
`blocks_expanded: 2000000` and a one-line warning, and nothing about how much was missing or what to do. PDF: nothing.

## 3. Can the true size be counted up front? Yes.

The expanded size is arithmetic over the block table: a copy of block B costs `1 + sum over its entities (1 + cost of
that entity)`, where a reference costs `cols x rows x (cost of one copy)`; a dimension block costs its entities; the
result is memoised per (block, depth, layer the layer-0 contents take) and the depth limit stops cycles exactly where
the build does. The walk follows what the build draws (hidden objects, layers off or frozen, "do not plot" layers when
plotting, a viewport's frozen layers, annotative objects hidden at the scale, attribute definitions not drawn), so the
count equals `DisplayList::expanded` of a build with no limit. Built as `cadcraft_render::count_space` /
`count_expansions` (crates/render/src/count.rs); test `the_up_front_count_is_what_an_unlimited_build_expands` checks
equality with an unlimited build on nested blocks (parallel path), arrays, layers that hide references and layer-0
inheritance, a self-referencing block, and a layout with viewports (one off, one with a frozen layer), also for plots.

Measured:

* Generated nested site layout (20 grids x 100 rows x 100 trees, 7 entities each): **1,804,020 expansions counted in
  17 microseconds**; the build of the same takes 1.5 s and about 390 MB of list. Counting costs nothing next to
  building, so it can be done before every draw and before any file is opened.
* The corpus (`/home/user/cad-corpus`, `/home/user/cad-work`, 146 DXF/DWG files read; 82 have block definitions): the
  **largest count is 2,220** (`acadsharp/samples/sample_AC1009_*.dxf`), then 1,503 and 1,486
  (`autodesk-samples/blocks_and_tables_*.dwg`), 817, 546, 480. **No file in the corpus is within three orders of
  magnitude of the limit**; the 1.5-million-entity `big/floorplate_500.dxf` has no blocks at all. The limit has only been
  reached by constructed bombs; the owner's site layout (tens of thousands of trees x 9 expansions each = 200 thousand
  to a few million) is the realistic case, and it is not in the corpus. (Survey tool: `BLOCK_SURVEY=dir1:dir2 cargo
  test -p cadcraft-io --test it block_survey -- --ignored --nocapture`.)

## 4. Options

**(a) No fixed limit: count up front, reserve from the count, refuse in words before any work.** Needs: the count
(built); a bytes-per-expansion estimate to turn the count into a memory reservation: the measured range is 36 B to
tens of KB per expansion, so the estimate must come from the blocks (build each distinct block once and scale) or be
a loose multiple, and the decision "does it fit" needs a memory budget, which is a new limit the owner has to agree.
Costs: a hostile bomb and a legitimate 5-million-tree site look the same to arithmetic (both are big numbers); only a
size threshold tells them apart, so something still refuses at some size. Gain: the refusal is before any work, exact,
and carries the ways through; memory is no longer a surprise.

**(b) Streaming expansion for outputs that do not need the list in memory.** The SVG writer already takes lists one
after the other in draw order. Today the slice is 16,384 top-level entities, so a slice can still hold the whole limit;
slicing by expansion (flush the builder every N primitives, at any depth, also inside a MINSERT) makes the held list
bounded by N whatever the drawing. PNG: the rasteriser's hairline batching is tested pixel-identical to one-by-one
drawing, so drawing slice by slice onto one pixmap in order is possible (the pixmap is width x height x 4 bytes, up to
16384 x 16384). PDF: slices can be drawn into the same page surface, but the PDF writer keeps the document in memory
(compressed content stream) and the file is returned as one `Vec<u8>`, so PDF memory still grows with the output.
Output is complete at any size, only time grows; time is what the limit also bounded, so a bomb is not stopped by
streaming. The owner's rule forbids a new timeout; the existing cancel (`ctx.cancel`) is the only stop. Needs a
builder sink (small in `Builder`: `list` is already a value that is drained and replaced), the writer and rasteriser
entry points per slice, and the up-front count to show progress and say "this will draw N expansions".

**(c) Views that draw less, by the caller's choice.**

* *A window*: **already so for model space in the MCP.** `Prepared::list_for` / `each_list_sliced` pick only the
  top-level objects whose extent reaches the window (`reaches`, rays and infinite lines always), and the limit is per
  build, so a window, or each tile of `tiles: {cols, rows}` with `output`, is a build of its own: a window that needs
  fewer expansions than the limit is drawn whole. Not so: (1) inside a block the whole block is expanded, even the
  copies of an array outside the window; (2) a layout's window only clips the writer (the layout is still all expanded) (the way to
  window a layout is `viewport: handle`, which renders that viewport's model view as a window; inside a layout build a
  viewport expands only the model objects reaching it); (3) **PDF does not cull by window**: `sheet()` calls
  `build_plot` of the whole space and the window only sets scale and placement, so `pages: [{window}]` (a book of
  frames) costs the whole drawing's expansions on every page.
* *Layers off* (`layers_off`, `layers_on`, `only_layers`): works today for every format, a reference on a layer that is
  off is not expanded.
* *Draw named blocks as their extents box*: **not built.** A render option (a set of block names; each reference draws
  the transformed outline of the block's extents instead of its contents, drawn and named in the answer). Needs the
  block extents (`doc/extents.rs` has them per block and depth), a polyline draw path in `insert`, a tool argument and
  the answer saying which blocks were drawn as boxes. Gives an overview of any size, always.

**(d) The route through PDF or tiles.** Tiles already work (above): each tile is its own build and file, complete if
it needs the limit or less; the answer now says so. For PDF the route needs (c)(3): when a window is given for model
space, build only the objects that reach it (a `build_plot` over the picked entities: it needs a plot variant of
`build_entities` carrying `prim_layers` and the plot flags), then `pages: [{window}, ...]` writes one PDF, one page per
window, each page its own build: one complete file at any size, memory per page. Cost: a small change in
`io::pdf::sheet` and the render crate, checked for PDF identical to today's when the window holds everything.

## 5. Recommendation

**(c) first, then (b) for SVG and PNG, keep a refusal before work, and have the agent steered at every step.**

1. *Now (built, A4 below):* the count in every answer, `blocks_not_drawn`, and the ways through in words.
2. *Next, owner's yes needed per item:* PDF culls by window for model space ((c)(3)/(d)): this is what makes the owner's
   PDF-of-tiles route real, gives one complete file at any size, and costs one pass over the entities. And the
   "blocks as extents boxes" option for a guaranteed overview.
3. *Then streaming (b) for the SVG writer by expansion and for PNG:* complete output at any size for the file formats
   that are written anyway. With streaming the limit's memory purpose goes; its time/bomb purpose stays, so the
   decision to keep, raise or replace the value (with the up-front count the refusal can be exact: "this needs
   N expansions, about X GB / Y s") is the owner's, with the question below.

Why not (a) alone: it replaces a fixed number with an estimate of bytes that varies 600-fold between drawings and so
needs a new memory limit that is itself a guess. Why not (b) alone: streaming does not make a 10^9 bomb finish; the
agent needs the count and the windows to choose. (c) is the one the agent can use today with no change in guarantees,
and it composes with (b).

## 6. Built now (A4)

* `cadcraft_render::count_space` / `count_expansions` (+ `count.rs`, viewport counting in `viewports.rs`; the
  viewport window/scale code was moved into a helper `View::of` shared by drawing and counting, no change of output).
* Every answer that draws (`render_preview` svg and png tile, `export` svg, png, pdf) carries
  `blocks_expanded_total`: the expansions the build asks for, counted before building. When it is past the limit:
  `blocks_truncated: true`, `blocks_expanded` (the limit), `blocks_not_drawn` (total minus limit), a warning, and
  `ways_through`: windows or tiles with the minimum number needed, layers off, and what a one-file drawing would need;
  a layout answer points at `viewport: handle`; a PDF answer says its window only places the page and sends the agent
  to svg/png windows or layer passes (it lists the sheets: `blocks_truncated_sheets`).
* PDF: `io::plot_with_blocks` returns the bytes and, per sheet, the count and the build's `expanded`; `plot` is
  unchanged. The PDF export answer says `blocks_truncated` like the others.
* Tool descriptions of `render_preview` and `export` say what the limit is and the ways through.
* Not changed: `MAX_BLOCK_EXPANSION` (2,000,000), what is drawn past it, the UI (still silent), the plotter callers.

## 7. Questions for the owner

1. Is a PDF that culls by window (so `pages: [{window}]` is complete per page) approved? It changes what is built for a
   PDF with a window (the picture is the same; fewer expansions).
2. Is "draw these blocks as their extents box" approved as an agent-chosen option (named in the answer)?
3. If streaming makes SVG/PNG complete at any size, what should stop a hostile bomb: keep a limit for it, and at what
   value, or the cancel alone? (No limit, timeout or fallback has been added.)
4. The UI shows no message when the canvas truncates; should it say so (a status-bar line from `count_space`)?

## 8. Layouts written in slices (part B)

`render::build_layout_slices(d, name, opts, slice, each)` hands over display lists that drawn in turn are the list
`build` makes of the layout: the sheet's own objects in slices of `slice`, and for each viewport, in stored order, its
border (as a sheet object) and its model-space contents in slices of `slice` objects, each clipped to the viewport
(rectangle or the clip boundary's outline) as the whole build clips them. What is carried from piece to piece: the
block-expansion count (the limit is the layout's, taken in draw order) and the pattern cells left for plot-style fills
(`pen::apply_budgeted`, the sheet's objects share one, each viewport's contents their own, as in the whole build).
The model-space slices are built on all cores when big, like `build_slice`. `Prepared::each_list_sliced` uses it for
a layout, so `export` svg of a layout is written slice by slice through the same atomic file write.

Fix found on the way: `viewports::contents` built a viewport's contents on all cores for a model of 512 or more
objects with no draw-order fallback, so past the limit the objects left out depended on thread timing; it now throws
the parts away and builds in draw order like `build_parallel` (the test fails without the fix).

Identical to today's: tests `a_layout_built_in_slices_is_the_whole_list` (render: the same primitives in the same
order and the same expansion count, 2,400 model objects with block references, a sheet with objects below and over a
rectangular viewport, a viewport clipped to a triangle and a viewport that is off, slices of 1, 7, 512 and 16,384,
under the limit, past it, and with a plot-style table with wide lines, ends, joins and a patterned fill) and
`a_layout_svg_written_in_slices_is_byte_identical_to_the_whole_list_one` (mcp: the SVG bytes). Not covered by a
different picture: viewports have no twist angle in this code (no rotated views), so every viewport kind the code has
is covered. Plots (PDF, plot-style layers, text glyph notes) are still built whole: slicing is for the screen build.
Not sliced: `render_preview` of a layout still builds the whole layout per tile (it returns the SVG inline).

## 9. The older SVG writer (part C)

`io::svg::write_list(out, list, window)` writes the SVG of a built display list to a writer as it goes: the header
first (the view box and decimals come from `list.bounds` or the window, known before any primitive), then the
primitives sent in blocks of 64 KB; a write error (disk full) comes back as an error. `export_list` keeps its String
signature and is now `write_list` into a `Vec` (callers that need bytes or a string are unchanged); test
`the_streaming_writer_makes_the_bytes_of_the_string_writer` compares with a verbatim copy of the old writer for
every primitive kind (opaque and transparent polylines, triangles, a wipeout mask, points, a ray and an infinite
line), with and without a window, a zero-size window, an empty list and 20,000 primitives. No caller in this
workspace writes a file with it (`write_framed` and the plotter's svg return bytes; the MCP uses its own `SvgWriter`),
so none was changed.

What it would take for those callers to also *build* the list in slices: `write_framed` svg and the plotter's
model-space svg call `build_plot` and then `pen::apply` with a scale that depends on the whole list's bounds
(`fit_scale(window or list.bounds)`), and the header's view box depends on `list.bounds` when no window is given: the
bounds are only known after the build. With a window (`write_framed`'s framed case, the plotter's paper bounds) they
are known before, so the build can be sliced exactly like `build_slice`; without one, either the bounds come from the
extents (`d.extents`, which is not the list's bounds: text and wide lines differ, so the picture would not be
byte-identical) or the build runs twice (once for the bounds). `build_plot` also needs a plot variant of `build_slice`
(the layers and text glyph notes the plot keeps). Not a small change; not built.
