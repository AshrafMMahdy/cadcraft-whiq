# The block-expansion limit (`MAX_BLOCK_EXPANSION` = 2,000,000)

Written on branch `4l13-round2b` (from `whiq` at 08af9f2). Part A of the round: what the limit is for, what the
agent sees, what can be counted up front, the options, a recommendation, and what was built now (A4). The limit's
value is unchanged. **Since 2026-11-11 (section 11) the picture tools no longer make a picture that passes the limit:
they refuse it, and they have a "blocks as boxes" option.** Sections 1 to 10 are the record of how it came to be;
where one of their sentences describes the cut picture, a note says what is true now.

## 1. What the limit is and what it protects

`Builder::expand()` (crates/render/src/lib.rs) takes one unit for every block copy (each cell of a MINSERT array) and
one for every entity drawn inside a copy; a nested reference is itself an entity and then expands its own copies.
Dimension and multileader blocks count the same way. When the units are spent, `expand()` returns false and the
remaining block contents are not drawn (the build still counts the calls it makes, so `DisplayList::expanded` goes
past the limit; `Options::blocks_truncated(expanded)` is that test). The doc comment on the constant names the reason:
a hostile or broken file with nested, self-referencing or arrayed references expands exponentially
(`MAX_BLOCK_DEPTH` = 16 only bounds the depth: a block holding four references to the next, 15 deep, is 4^15 line
copies; one MINSERT of 10,000 x 10,000 is 100 million copies). It is CWE-400 protection. (That is what the build
library does. The MCP picture tools, `render_preview` and `export` to svg / png / pdf, never use the cut list since
2026-11-11: they count first and make no picture past the limit, section 11. The canvas and the plotter still keep what
the build drew.)

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
| `render_preview` svg (preview.rs `list_for`, `svg_tile`) | the whole display list of the tile, then the whole SVG `String` of the tile (and the JSON answer when it is returned inline) | one tile | refused before drawing (section 11) |
| `render_preview` png (`preview::png`) | the display list of the tile plus the pixmap | one tile | refused before drawing (section 11) |
| `export` svg model space (`write_svg_sliced`) | one slice of 16,384 top-level entities (`SLICE_ENTITIES`) as a display list, dropped before the next; the file is written as it goes | one slice (but a slice whose entities expand a lot is up to the whole limit: slicing is by top-level entity, not by expansion) | refused before drawing (section 11) |
| `export` svg of a layout | one slice at a time since this round (`build_layout_slices`: the sheet's objects and each viewport's contents in draw order); before it the whole layout list | one slice | refused before drawing (section 11) |
| `export` png | as `render_preview` png | the export | refused before drawing (section 11) |
| `export` pdf (`io::pdf::plot` -> `sheet`) | one `DisplayList` per sheet (`build_plot` of the whole space; the plot window only places the drawing on the page, it does **not** leave out block references outside it), then the PDF writer's own document in memory, then the `Vec<u8>` | per sheet; the document until the file is written | **nothing before round 2b** (the answer never said); now every sheet is counted first and one sheet over the limit means no file (section 11) |
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
* *Draw named blocks as their extents box*: **built 2026-11-11** (section 11, `blocks_as_boxes`). A render option (a
  set of block names; each reference draws the transformed outline of the block's extents instead of its contents,
  drawn and named in the answer). Gives an overview of any size, always.

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
   PDF-of-tiles route real, gives one complete file at any size, and costs one pass over the entities (still open).
   The "blocks as extents boxes" option for a guaranteed overview was approved and built on 2026-11-11 (section 11).
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
  `blocks_expanded_total`: the expansions the build asks for, counted before building. *(When it was past the limit
  this round's answer said `blocks_truncated: true`, `blocks_expanded`, `blocks_not_drawn`, a warning, `ways_through`
  and, for a PDF, `blocks_truncated_sheets`, and a cut picture was written. Replaced on 2026-11-11: section 11.)* The
  ways (windows or tiles with the minimum number needed, layers off; a layout answer points at `viewport: handle`; a PDF
  answer says its window only places the page and sends the agent to svg/png windows or layer passes) are now the
  body of the refusal.
* PDF: `io::plot_with_blocks` returns the bytes and, per sheet, the count and the build's `expanded`; `plot` is
  unchanged. The PDF export answer says `blocks_truncated` like the others.
* Tool descriptions of `render_preview` and `export` say what the limit is and the ways through.
* Not changed: `MAX_BLOCK_EXPANSION` (2,000,000), the UI (still silent), the plotter callers. (What is drawn past the
  limit changed for the picture tools on 2026-11-11: nothing is drawn, section 11.)

## 7. Questions for the owner

1. Is a PDF that culls by window (so `pages: [{window}]` is complete per page) approved? It changes what is built for a
   PDF with a window (the picture is the same; fewer expansions).
2. Is "draw these blocks as their extents box" approved as an agent-chosen option (named in the answer)? **Yes,
   2026-11-11, built (section 11).**
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

`io::svg::write_list(out, list, window)` writes the SVG of a built display list to a writer as it goes: the text is
made primitive by primitive and sent whenever it reaches 64 KB (the header, whose view box and decimals come from
`list.bounds` or the window and are known before any primitive, opens the first block); a write error (disk full)
comes back as an error. `export_list` keeps its String
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

## 10. Round 2b: what the blind review changed

*(Section 11 replaced the answer's vocabulary: where this section says `blocks_truncated`, `blocks_not_drawn`,
`blocks_expanded` (the limit) or a picture with the first expansions drawn, read "the picture is refused" and the
fields of section 11. The rules below that still hold are marked.)*

A picture-making answer never tells the agent to change the customer's drawing: no route that edits it (explode, arrays
rebuilt as separate references, undo, close with discard) is offered anywhere. *(Still true.)* Editing routes were written and removed:
they alter customer data, have limits of their own (arrayrect refuses very large arrays), lose properties and are not
safe to undo blind.

**The build is the ground truth; the count explains it.** Every answer that draws takes `blocks_expanded_total` from the
build's own `DisplayList::expanded`. *(Until 2026-11-11 `blocks_truncated` was true only when that count went past the
limit and the total was then the count from the block table. Now the count from the block table
(`render::census_space` / `census_entities`) decides before anything is drawn, because it is proven equal to the build;
the build's own count is still checked and a picture whose build passed the limit is thrown away, never used.)* A count
and a build that disagree under the limit give the build's figure. Tests: mcp `the_build_decides_whether_an_answer_is_truncated` and
`a_picture_of_annotative_dimensions_is_still_drawn` (41 annotative dimensions whose block table count is
2,050,000 > the limit; the build expands 0; render_preview, export svg and export png do not say truncated).

**Annotative dimensions.** At depth 0 a non-hidden annotative object goes through `anno_scaled`, which sets a
dimension's `block` to `None`: the build expands nothing for it. The counter takes the object's `AnnoDraw`
(`Counter::top`): hidden costs 0, a scaled dimension costs 0, scaled multileaders and references keep their block (a
dimension nested in a block is not scaled and keeps its block). Render test
`annotative_dimensions_count_what_the_build_expands` (model space, layout, viewport; equal to the unlimited build).

**Dimension blocks are memoised** in their own map (no unit for the copy, AttDefs included) on (block, depth, layer), so
a dimension block holding k dimensions of itself costs 16 x k visits, not k^16 (k = 10 is 11,111,111,111,111,110
expansions, counted at once: render `a_dimension_block_of_dimensions_is_counted_at_once`). The counter checks the
build's `Options::cancel` at every entity. The memo key no longer clones the layer name per visit (two-level map looked
up by `&str`).

**One object over the limit (windows cannot split it).** The counting pass finds the largest single top-level object
with its handle, kind and block (`BlockCensus::largest`; answer field `largest_object`). When it needs more than the
limit, the answer says that windows and tiles cannot complete it (a build expands every object that reaches its window
whole) and does not advise windows. It offers only routes that leave the drawing as it is:

1. *Layers.* Switching off the layer the **references** are on (answer fields `reference_layers` and
   `layers_that_reduce`, each with what the count becomes and whether that fits one build; layer names compare ignoring
   case, and a layer missing from the layer table is not offered). The build takes one expansion for every entity of a
   block before it looks at that entity's layer, so switching off the layers of the block's contents reduces nothing.
   When every reference is on one known layer the answer says no split into passes exists. The references on the layer
   switched off are then not in the picture.
2. *Reading* (`read_drawing`, `search_drawing`, measuring, extents) does not expand blocks, so the information stays
   reachable.

*(Until 2026-11-11: if no layer left a build that fit, the answer said "no complete picture of this object can be drawn
today; what is drawn is the first N expansions in draw order". Now nothing is drawn, and the refusal also offers the
blocks as boxes; it says "none of these alone gives a complete picture" when no way alone fits, section 11.)* Tests: mcp
`one_object_over_the_limit_is_named_and_only_routes_that_leave_the_drawing_are_offered` (names the 30 x 30 array, tiles
stay truncated, the layer figure given equals the `expanded` of a build with that layer off, no editing words in the
answer, the plain statement when no layer fits), render `only_the_layer_of_the_references_reduces_the_count`,
`reference_layers_ignore_case_and_leave_out_layers_missing_from_the_table`; mcp
`the_layer_advice_names_the_reference_layers_and_it_reduces_the_build`. Windows are advised only when the largest object
fits the limit: mcp `passing_the_block_cap_is_reported_with_the_ways_through` ("at least 7 windows", each tile under
the limit drawn whole).

**Drawings with no complete picture today** *(this list is the state before 2026-11-11; blocks as boxes now give a
complete overview of the first, third and fourth kinds when the boxes fit; PDF by window and streaming are still not
built)*:

* One block reference or array that alone exceeds the limit and whose references sit on one layer (or on layers that all
  still leave a build over the limit): windows cannot split it, layers cannot split it; reading is the only route.
* A PDF of a space past the limit: one sheet is one build, so no PDF holds it all (SVG / PNG windows can).
* One paper-space sheet past the limit as a single picture (SVG, PNG or PDF page): the layout is built whole. The model
  space its viewports show is complete in tiles (`viewport: handle`), the sheet itself with its title block is not.
* A reference whose nesting multiplies past what can be counted (10 levels of 1,000 = 10^30).
* An array with more than 10,000 columns or rows: drawn with 10,000 (the clamp in `blocks::insert`, unchanged: the
  owner decides); the answer says so (`arrays_clamped`) and that no way to draw more exists today.

**Saturated and huge counts.** A count that overflows `u64` is `COUNT_SATURATED` (`u64::MAX`); the answer then says
"more than can be counted (nested references multiply past 1.8e19)" in the field and the reason, gives no number of
windows and never prints the saturated value. (`blocks_not_drawn` no longer exists; `blocks_expanded_total`,
`largest_object.blocks_expanded_total`, `layers_that_reduce[].blocks_expanded_total_if_off` and
`heaviest_blocks[].blocks_expanded_total_if_boxed` are the fields that can be a number, a string or the words.) Any count above 2^53 is a decimal string; up to it a number. Fields that
can be a number, a string or the words: `blocks_expanded_total`, `blocks_not_drawn`, `largest_object.blocks_expanded`,
`layers_that_reduce[].blocks_expanded_total_if_off`; documented in the `render_preview` and `export` descriptions (mcp
`the_descriptions_document_the_number_or_string_fields`). Behaviour: mcp `a_count_that_overflows_answers_the_words_and_no_number`;
render `the_census_names_the_largest_object_clamped_arrays_and_overflow`.

**PDF.** *(Replaced by section 11: `plot_complete`, `SheetBlocks { sheet, number, total, limit, census }`, and the
refusal's `sheets_over_limit` / `sheet_numbers_over_limit`.)* `plot_with_blocks` / `SheetBlocks` carried each sheet's
`total` (the build's count when drawn whole, else the count), `expanded`, `limit` and the census. The answer had
`sheets: [{sheet, blocks_expanded_total, blocks_not_drawn, and for a truncated sheet its own largest_object,
reference_layers, layers_that_reduce}]`, totals that were the sums of them, and at the top level the largest object,
reference layers and ways of the sheet that needs the most (`details_of_sheet` named it). Tests: mcp `a_pdf_note_gives_the_sheets_figures_and_they_add_up`; io
`a_pdf_plot_past_the_limit_says_how_much_was_not_drawn` (a real plot past a lowered limit,
`PdfOptions::max_block_expansion`, not read from JSON).

**Arrays drawn short.** Every answer that draws lists them (`arrays_clamped: [{handle, block, cols, rows, drawn_cols,
drawn_rows}]` and a note); mcp `an_array_drawn_short_is_reported`.

**Layout slices.** Render `a_layout_built_in_slices_is_the_whole_list` now covers frozen layers, `skip_viewport`,
annotative objects and a wipeout through a viewport, and more viewports than are drawn; mcp
`a_layout_svg_written_in_slices_is_byte_identical_to_the_whole_list_one` also runs past the limit.

**Cost.** The census runs once per tile on the objects the tile's build already picked (it used to pick them again), and
once per call for a layout. Measured on 4 x 4 tiles of 400,000 objects (mcp `counting_cost_per_tile`, ignored): 3.25 s
for picking twice and counting, 1.45 s for picking once and counting from it. The per-layer recounts that fill
`layers_that_reduce` run only when the build went past the limit, one walk per layer the references are on.

**Streaming writer.** Nothing in the workspace writes a file with `svg::write_list` (the plotter's SVG and
`write_framed` return bytes, as their callers expect), so none was changed; the doc comments now say what it does.

## 11. 2026-11-11: a picture over the limit is not made; blocks as boxes (4L13 item 3)

The owner's decision. The limit stays 2,000,000 and stays where it is (`MAX_BLOCK_EXPANSION`, `crates/render/src/lib.rs`).
It concerns **only making a picture**: the MCP tools `render_preview` and `export` to svg, png and pdf. Opening,
reading (`read_drawing`, `search_drawing`, takeoff), measuring, extents and editing a drawing are untouched and say
nothing about it. In the answers the unit is called a **block piece**: every copy of a block (each cell of a MINSERT
array) and every object in it, nested blocks counted; the field names keep `blocks_expanded_*`.

**Why.** A cut picture looks like a finished one. The agent (or the customer it reports to) can take a plan with its
trees, symbols or fixtures silently missing for the whole plan, and a warning in a field is easy to lose. A picture is
now either complete or not made.

### What changed

1. **Over the limit, nothing incomplete is produced.** One picture is one build: a `render_preview` tile, an `export`
   svg or png of one space, one PDF sheet. When it needs more than the limit, **no picture is returned and no file is
   written**, and a file already at the output path is not touched, renamed to `OLD_` or removed (the count is made
   before the file is opened). The answer is a refusal in the engine's refusal form (`code: too_large`, `reason`,
   `next_step`) that says: how many block pieces the picture needs (`blocks_expanded_total`) and that one picture draws
   at most 2,000,000 (`blocks_expanded_limit`); what is left if each reference layer is switched off, and whether that
   fits (`layers_that_reduce`, as before); the heaviest blocks by name, each with what is left if that block is drawn
   as outline boxes and whether that fits (`heaviest_blocks`), the biggest saving first, ending where drawing all the
   listed blocks as boxes fits (`heaviest_blocks_all_boxed`); when even every block as boxes does not fit, every block
   that saves anything is listed and it says so; windows or tiles where the existing rules say they help (the
   truthful wording stays: an object larger than a window is expanded whole in every window touching it, a layout is
   built whole, a PDF sheet is not culled by its window), the largest object when one object alone needs more than
   the limit, and reading. `next_step` carries all the ways in words, numbered; the structured fields carry the
   figures. Under the limit the answer is what it was (`blocks_expanded_total` only, plus `arrays_clamped`) and the
   SVG / PNG / PDF bytes are the same as before.
2. **Tiles.** Each tile is its own picture. With `output` the tiles that fit are written; a tile over the limit is not,
   and its entry says `picture_made: false` with the whole refusal under `refused`. Inline, the same entries carry no
   image. With one picture asked for (one tile, or no tiling) the refusal is the call's error. `export svg|png` of
   several spaces does the same per space.
3. **PDF.** One file: every sheet is counted before any is drawn; if any sheet is over the limit no PDF is written and
   the refusal names the sheets over it with their page numbers (`sheet_numbers_over_limit`, `sheets_over_limit` with
   each sheet's own figures and tables, `sheets_within_limit`). `io::plot_complete(d, space, opts, limit)` returns
   `Plot::TooBig(sheets)` or `Plot::Drawn(..)`; `io::plot` (the engine's `plot` hook) is unchanged and still cuts.
4. **Removed vocabulary.** `blocks_truncated`, `blocks_not_drawn`, `blocks_expanded` (as the limit), `warning`,
   `ways_through` (the field; the text is `next_step`), `blocks_truncated_sheets`, `details_of_sheet`,
   `SheetBlocks::truncated` / `not_drawn`, `io::plot_with_blocks`. No answer describes a cut picture any more.

### Decided before drawing, and what stays the ground truth

The up-front count is the decision wherever it is proven equal to what the build expands: the equality test
`the_up_front_count_is_what_an_unlimited_build_expands` (render) checks `count == unlimited build` for nested blocks
(parallel path), arrays, hidden and layer-0 contents, self reference, layouts with viewports, annotation, plots, and now
also with every block, one at a time and all together, drawn as boxes; so a picture that the build would draw whole is
never refused. The case of section 10 (41 annotative dimensions whose block table holds 2,050,000 pieces while the
build expands none, because an annotative dimension is drawn from its definition) is counted as 0 by the walk
(`Counter::top`), so the picture is drawn; the test `a_picture_of_annotative_dimensions_is_still_drawn` runs it through
render_preview and export svg / png. The build's own count is still checked after the build: if it passes the limit
(which the equality test says cannot happen) the picture is thrown away and the error says so; nothing incomplete is
ever returned or written.

### Blocks as boxes (`blocks_as_boxes`)

`render_preview` and `export` (svg, png, pdf) take `blocks_as_boxes`: a list of block names, compared as block names are everywhere in the engine (ignoring case; `Drawing::block`). Every reference to a
named block, at any nesting depth, is drawn as **one outline** of the reference's extents on the reference's own layer
and colour, instead of its contents, and costs **one** block piece. The extents are the block's contents' extents
(`doc::block_contents_bounds`, the inner part of the reference extents of `doc/extents.rs`: layers off left out) under
the reference's transform, so a rotated or scaled reference gives a rotated rectangle; a MINSERT array is one outline
of the whole array (the box of its four corner copies in the turned frame, turned forward again), not one per cell.
The up-front count and the build agree with the option on (the extended equality test). The answer names
`blocks_drawn_as_boxes: [{block, outlines_drawn}]` (outlines actually drawn: nested ones once per copy of the block that
holds them; a block with nothing visible in it draws none; a PDF gives it per sheet and summed). A name that is not a block of the drawing is refused in words naming
it (`not_found`, with the nearest block names, where the full list is and to call again with names from it), nothing
drawn or written. Anything that is not a list of block names (a number, `[123]`, an object, a `null` in the list) is
refused naming what was given, with the expected form and an example, on every path that can receive it (the tools, the
PDF JSON, the engine's plot hook; the png / svg plotter says it does not draw boxes). The drawing is never changed.

**What a box replaces.** A boxed reference draws one outline and nothing else: its contents AND its attribute texts are
not drawn (decided: attribute texts on millions of references are themselves a load the option exists to remove). The
answer's note says so. **What the outline covers.** The extents are taken once per block, from the drawing the picture
is built from (layers off or frozen in the drawing are left out, as are the layer switches of the call and the frozen
layers of a `viewport:` target). Layers frozen only in one layout viewport, and layers marked do-not-plot in a PDF, are
not subtracted: the box would have to be different per viewport and per plot, and the extents code has one rule set
that cannot take those without a second copy of it. So there the outline can be larger than what the block would draw,
never smaller (the answer's note says so).

### How the heaviest-blocks list is found, and what it costs (measured, round 3)

**Design.** The count walks the block table once (memoised). When the picture is over the limit, or blocks are
drawn as boxes, one more walk records every reference with its copies and the cost of one copy. Everything per
block and per layer then comes from that recording by arithmetic and never from a walk per name: the expansions
form a graph whose edges raise the depth by one; taking away a set of references (a block's references drawn as
boxes, a layer's references switched off) changes the cost of one copy of every expansion that holds one and of
those above it, found from the cut upwards, and the top level sums the same. A block that holds itself, or a layer
whose references hold others on the same layer, cost no more than any other, and the outlines drawn per boxed block
come from the same recording (the box sites carry the block name). A figure that overflows is "more than can be
counted": it is listed (first, as the heaviest), it is not walked, and it gets no after-figure; when the total is a
number every figure is exact. The only counts besides the first two are the few that find how many of the heaviest
blocks, boxed together, fit (found by halving: about log2(blocks) walks of the objects) and one to state the figure
with all the listed blocks boxed. A saturated total still lists the blocks that save (with their saving only), so
n independent overflowing blocks, which fit when boxed together (n pieces), are offered as that. The layer table of
a census is built once (`Drawing::layer` is a linear scan).

Test `the_refusal_grows_with_the_drawing_not_with_its_square` runs every shape below with a bound, and
`heaviest_equals_walks` asserts that every figure that is given (block and layer, order, after-figure) equals a real
count of a drawing with that block boxed or that layer off, on every fixture including a block holding itself.

Measured (release build, one core, test `refusal_shapes`; the limit set so every table is made; tables only):

| drawing | tables (was) |
|---|---|
| 250 / 1,000 / 2,000 / 4,000 / 20,000 blocks each holding two references to itself | 0.013 / 0.06 / 0.22 / 0.35 / 2.1 s (0.54 / 9 / 43 s for 250 / 1,000 / 2,000) |
| 125 / 250 / 500 / 1,000 / 4,000 blocks each holding 20 references to itself (every saving overflows) | 0.06 / 0.11 / 0.14 / 0.33 / 1.8 s (1.07 / 4.7 / 20 / 88 s) |
| the same, each reference on a layer of its own | 0.08 / 0.09 / 0.21 / 0.50 / 2.5 s (1,000: 241 s) |
| 4,000 blocks x 30 references plus one block holding 20 references to itself (the total overflows) | 0.12 s (150 s) |
| 300,000 references over 1,000 / 5,000 / 20,000 layers | 0.32 / 0.36 / 0.46 s (3.4 / 22 / 104 s) |
| 20,000 layers, each with a reference to one block holding 25 / 100 references to a leaf block | 0.8 / 2.1 s (18 s for the verifier's shape) |
| 1,000 / 4,000 blocks drawn as boxes, success path (the outlines tally) | 0.02 / 0.10 s (5 s / 80 s) |
| 50 blocks x 2,000 refs / 500 x 1,000 / 5,000 x 100 / 20,000 x 30 (500,000 to 600,000 objects) | 0.16 / 1.2 / 1.6 / 2.1 s |
| 2,000 blocks x 300 refs, nested (600,000 objects, total 1.1e14) | 3.8 s |

The list can therefore be very long (thousands of entries) when a drawing is far over the limit with many block types.
**No cap was put on it**; the answer is bounded by the existing `max_result_bytes` instead: the heaviest are shown first
as far as it allows (`heaviest_blocks_more` says how many more; boxing the listed ones and calling again lists the next
ones), the layer table likewise (`layers_that_reduce_more`), and each name is said once in the words and once in the
field. (No real drawing of the corpus is anywhere near the limit, section 3, and the corpus is not on this machine, so
these are generated drawings.)

### Not changed, and why

The value of the limit; the desktop canvas and the plotter and engine `plot` callers (they still keep what the build
drew; the canvas options only gained the field `blocks_as_boxes: None`); PDF culling by window; streaming output; the
10,000 columns / rows clamp of a MINSERT (`arrays_clamped` is still said in every answer that draws one short: that is
a cut picture of another cause, not named in the decision). No timeout, cap or default was added. One check was added behind the count: if a build still goes past the limit although the count said it fits (the equality test says it cannot), no picture is made and the answer says, in words, that the picture came out over the limit although the count said it fits, that the count may be wrong for this drawing, what both figures are, and to switch off more layers and / or draw more blocks as boxes and try again (a PDF sheet the same way, with its number). It changes no result when the count and the build agree; the owner decided it stays.

Tests: render `a_boxed_reference_is_one_outline_of_its_extents_under_its_transform`,
`a_boxed_block_nested_in_another_is_boxed_at_every_depth`, `the_heaviest_blocks_are_listed_down_to_where_boxing_them_fits`,
the extended equality test; io `a_complete_pdf_plot_past_the_limit_makes_no_file`,
`a_pdf_of_several_sheets_with_one_over_the_limit_makes_no_file`; mcp `a_picture_over_the_limit_is_refused_and_nothing_is_written`
(svg, png, with output, export svg / png / pdf; existing files untouched; layer off, boxes, still over after a choice),
`tiles_that_fit_are_written_and_a_tile_over_the_limit_is_refused_on_its_own`,
`a_pdf_with_a_sheet_over_the_limit_is_not_written_and_names_the_sheets`, `an_unknown_block_name_is_refused_naming_it`,
`under_the_limit_the_answer_is_the_count_and_nothing_else`, the descriptions test. The tests lower the limit through
`Options::max_block_expansion`, `PdfOptions::max_block_expansion` / `plot_complete`'s `limit`, and (mcp, test builds
only) `preview::lower_limit`.

### Round 2 (blind review) in short

* Round 2 removed the quadratic walk for blocks that hold themselves and for thousands of layers; it was not yet linear
  for every shape (a walk per overflowing block or per layer with shared content remained). Round 3 (below) made it so:
  every shape is measured in the table above.
* The PDF count stops on a cancel (the tools pass theirs) and counts identical (space, boxes, limit) sheets once.
* A layout over the limit is refused once for the call (it is built whole for every tile); refused tile entries count
  against `max_result_bytes`; the heaviest list is said once in the words (`next_step`) and once as `heaviest_blocks`,
  the example does not repeat the names.
* The engine commands `plot`, `export` (svg / png / pdf), `publish`, `pdfbook` and `preview`, reached through `edit`,
  count first (`Session::complete_pictures`, set by the MCP only; the desktop app is unchanged) and over the limit write
  nothing and say to use the `export` tool with `blocks_as_boxes` / `layers_off`. `publish` and `pdfbook` count every
  sheet before the first file. `pngout`, `jpgout` and `bmpout` are not commands of this engine (edit says unknown
  command), so nothing can be written through them.
* Every refusal steers: it says what was wrong, what to give instead (with an example) and to call again.

### Round 3 in short

* The refusal and the success path never cost a walk per name (design above); the heaviest and layer lists are bounded by
  `max_result_bytes`; a saturated total lists the blocks that save instead of claiming nothing helps.
* A stop during drawing ends the call as cancelled with no file, for svg, png, the tile loop and PDF (the plotter checks
  the stop after every sheet).
* The engine commands that can write or return an image, an SVG or a PDF all follow the rule through `edit` (counted
  first, nothing written over the limit): `export`, `plot`, `publish`, `pdfbook`, `preview`, `wblock`, `qsave` / `save`,
  `saveas`, `document.bytes`, `layout.saveas` (the last five by the file's extension or format). `blocks_as_boxes` is drawn
  only by `plot` to PDF (counted with the boxes and written with them); every other command refuses it naming the `export`
  tool, so no picture is counted one way and drawn another. Names are checked as the tools check them.
* `blocks_as_boxes` is a list of block names only; a string, a number or a list with a non-string is refused naming what
  was given and the form expected. No default stands in for a wrong value.
