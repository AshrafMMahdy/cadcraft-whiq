# WHIQ changes: CADCraft as the platform's DWG/DXF engine

Branch `whiq` of the CADCraft fork. CADCraft becomes the White Hat IQ platform's reader, Preview
renderer, exporter and editor for DWG and DXF: `cadcraft-cli mcp` runs a headless MCP server over
stdio in the shape the platform's craft engines share (the forks' `CONTRACT-PARITY.md`: document
handles, jobs, `busy`, `cancel`, `server_status`, recovered files, `--automation-*-root`
confinement, OLD_ copies, `_meta["whiq/files"]`). `--connect HOST:PORT` still bridges to a running
app with the upstream tools.

The owner was away when this was built; every choice below was made by a score table and is
recorded here for review.

## What changed

| Where | What |
|---|---|
| `crates/mcp/src/whiq/` | The engine: `runtime.rs` (stdio loop, worker thread, jobs, busy, cancel, server_status, eviction, checkpoints, recovered files, signals, progress notifications), `contract.rs` (error codes, forgiving names and arguments, trimming with `continue_from`; the forks' shared shape), `files.rs` (roots, atomic writes, OLD_ copies), `docs.rs` (document handles), `read.rs` (read and search), `preview.rs` (Preview renderer), `tools.rs` (tool definitions and the domain tools) |
| `apps/cadcraft-cli` | `mcp` takes the contract flags (below); headless `mcp` serves the engine |
| `crates/io` | `read_limited`: a read with the DWG bridge's limits chosen by the caller (the engine sizes them from its memory budget) |
| `crates/engine` | `ReadFile::from_drawing`: hand a drawing the host read itself to `finish_open` |
| `crates/dxf` | Binary DXF of R12 and earlier (1-byte group codes) reads; it failed with "no sections found" |
| `.github/workflows/whiq-release.yml` | Builds `cadcraft-cli` for Linux x86_64 (musl, static) and Windows x86_64 (static CRT) with craft-fonts at the shared pin, runs the contract tests and the smoke against the built binary, publishes `cadcraft-cli-<tag>-linux-x86_64.tar.gz`, `cadcraft-cli-<tag>-windows-x86_64.zip`, `SHA256SUMS`, `engines.lock` |
| `apps/cadcraft-cli/tests/mcp_contract.rs`, `whiq_smoke.py` | The contract suite (every tool, on drawings it builds; `CADCRAFT_CLI=path` runs it against another binary) and the platform's build smoke |

## Flags of `cadcraft-cli mcp` (the forks' names)

| Flag | Default |
|---|---|
| `--automation-read-root DIR`, `--automation-write-root DIR` | none (unconfined) |
| `--automation-no-old-copies` | off (OLD_ copies kept) |
| `--author NAME` | none (stamped as `$LASTSAVEDBY` on saves; a call's `caller` overrides) |
| `--busy-after-ms N` | 300 |
| `--job-after-ms N` | 15000 |
| `--open-job-mb N` | 20 |
| `--idle-evict-secs N`, `--max-memory-mb N` | off |
| `--max-result-bytes N` | 262144 |
| `--checkpoint-edits N`, `--checkpoint-secs N` | 20, 60 (0 turns either off) |

## Tools

Every tool also takes `caller`, `async` (a job at once), `continue_from` and `max_bytes`. Errors are
`isError: true` with `structuredContent.error = {code, reason, next_step, …}`, codes from the
forks' 18. Answers over `--max-result-bytes` are trimmed (`truncated`, `continue_from`, `total`).

| Tool | Arguments | Answer |
|---|---|---|
| `open_drawing` | `path` (read root) | `{document, name, path, format: dwg\|dxf, version, release, entities, layouts, layers, blocks, warnings}` |
| `new_drawing` | `metric?` | `{document, …}` |
| `read_drawing` | `document?` \| `path?`; `section`: summary \| text \| dimensions \| title_block \| entities \| layers \| blocks \| layouts \| xrefs; `space?` (all \| model \| paper \| layout \| list), `layer?`, `window?` [[x0,y0],[x1,y1]], `contains?`, `match?`, `type?`, `expand_blocks?` (true), `geometry?`, `anonymous?`, `limit?` (500), `continue_from?`, `format?` json \| markdown | summary: `{file {format, version, release, bytes}, units {insunits(_name), measurement, lunits(_name), luprec, aunits, dimscale, ltscale}, entities, counts {space: {type: n}}, texts, dimensions, layouts [{name, entities, extents, paper, viewports, active}], layers {count, off, frozen, names}, blocks, xrefs, title_blocks, styles, read_notes, skipped_records}`; lists: `{total, returned, offset, <section>: […], continue_from?}` — text `{handle, type, text, layer, space, at, height, rotation, style, tag?, block?, insert?}`; dimensions `{handle, kind, text, measurement, unit, overridden, layer, space, at, style, points}`; title_block `{candidates [{handle, block, space, layer, at, scale, rotation, score, likely, attributes {TAG: value}, block_texts}], sheet_texts}`; entities `{handle, type, dxf, layer, space, color, bounds, block?, in_block?, geometry?}`; layers `{name, on, frozen, locked, plot, color, linetype, lineweight, transparency, description, entities}`; blocks `{name, entities, base, references, attributes [{tag, prompt, default}], xref_path}`; layouts `{name, extents, entities, tab_order, page, viewports [{handle, center, width, height, view_center, view_height, scale, frozen_layers}], active}` |
| `search_drawing` | `query`, `match?`, `kinds?` (text, attribute, tag, dimension, block_reference, block, layer, layout), `space?`, `layer?`, `window?`, `limit?` | `{total, hits [{match, kind, text, tag?, handle, layer, space, at, block?, insert?}]}` |
| `render_preview` | `document?` \| `path?`; `space?` (model) \| `viewport?` (handle); `window?`; `tiles?` {cols, rows}; `tile?` [col, row]; `layers_off?`, `layers_on?`, `only_layers?`; `format?` svg \| png; `width?`, `height?`; `background?` light \| dark; `output?` (write files) | `{space, format, window, extents, total_tiles, returned, tiles [{tile, window, bytes, primitives \| width/height, svg? \| saved?}], primitives, layers [{name, on}], continue_from?}`; PNG tiles come as image blocks |
| `export` | `path` (its extension or `format`: dxf \| dwg \| pdf \| svg \| png), `document?` \| `source?`, `version?` (2000–2018), `binary?`, `space?` (model \| layout \| list \| all), `window?`, layer switches, `paper?`, `landscape?`, `scale?`, `fit?`, `lineweights?`, `width?`, `overwrite?` | `{document, format, saved [{path, bytes, format, space?, previous_renamed_to?}]}` |
| `edit` | `document?`, `space?`, `atomic?` (true), `ops` [ `{op: command, command, params}` \| `{op: cmdline, text}` \| `add {type, …}` \| `delete {handles}` \| `set_text {handle, text}` \| `set_attributes {handle \| block, values}` \| `set_properties {handles, …}` \| `layer {action: new\|set\|delete\|current\|rename, …}` \| `block {action: define\|insert\|delete\|rename, …}` \| `space {name}` \| `undo` ] | `{document, applied, failed, results [{index, op, ok, result \| error}], entities, dirty}`; atomic failure: error with `failed_at`, `rolled_back`, `results` |
| `save_drawing` | `document?`, `path?` (default: its name in the write root), `version?`, `binary?`, `overwrite?` | `{document, …, saved [{path, bytes, format, previous_renamed_to?}], version}` |
| `close_drawing` | `document?`, `discard?` | `{closed, open_documents}`; unsaved changes refused (`invalid_request`) without `discard` |
| `list_commands` | `filter?`, `limit?` | `{commands [{id, label, aliases, params}]}` |
| `roundtrip_check` | `document?` \| `path?`, `format?` dxf \| dwg, `version?` | `{lossless, problems [{what, space?, type?, before, after}], bytes}` |
| `server_status`, `job_status`, `list_jobs`, `cancel`, `evict` | the forks' shapes | the forks' shapes; `server_status` adds `formats` |

## Decisions (score tables; owner to review)

Scores are 1–5, higher is better; weights in brackets.

1. **Where the engine lives.** (a) a `whiq` module in `cadcraft-mcp`, headless `mcp` serves it;
   (b) a new crate; (c) extend the upstream tools in place.
   Contract fit [3]: a 5, b 5, c 2 · upstream merge friction [2]: a 4, b 5, c 2 · effort [1]: a 5, b 3, c 3 → **a 28**, b 28, c 13. Tie broken by fewer moving parts: (a). `--connect` keeps the upstream server.
2. **DWG → DXF route.** (a) through CADCraft's model (the route Save As takes; edits included);
   (b) the DWG bridge's own DXF bytes when unedited. One route, always consistent with what read
   and preview show [3]: a 5, b 2 · fidelity of unmodelled objects [2]: a 4 (kept raw), b 5 ·
   no second path [2]: a 5, b 1 → **a 33**, b 18.
3. **Recovered files.** Always DXF (`<name>.recovered-<stamp>.dxf`), CADCraft's lossless native
   format, whatever the source. Writing DWG in a signal handler goes through the third-party
   writer [reliability 3]: dxf 5, dwg 3 · same extension as the source [1]: dxf 2, dwg 5 → **dxf 17**, dwg 14.
4. **Preview output.** (a) SVG tiles in the answer, paged with `continue_from`, never cut inside
   a tile; `output` writes files on request; (b) always files in the write root (the supervisor
   would mirror every preview into the chat's outputs). No clutter in outputs [3]: a 5, b 1 ·
   big drawings [2]: a 4, b 5 · no caps [2]: a 5, b 5 → **a 33**, b 23.
5. **Size limits.** No cap on the file; the DXF rendition of a DWG is bounded by the largest of
   the old 1 GB, 16× the file and half of `--max-memory-mb` (the bridge allocates from sizes a
   damaged file declares; a runaway conversion must stop). The old fixed 512 MB DWG cap is gone in
   the engine (the app keeps it).
6. **Edit shape.** One `edit` tool with a list of operations (any command, plus shorthands), all
   or nothing by default, against one tool per operation [calls per change 3]: list 5, per-op 1 ·
   schema clarity [2]: list 3, per-op 5 · coverage of every command [2]: list 5, per-op 2 → **list 31**, per-op 17.
7. **Texts inside blocks.** Read by default (`expand_blocks: true`), with the block path and the
   reference's transform: construction drawings keep tags, notes and title blocks in blocks.
8. **Release workflow.** A separate `whiq-release.yml` (as the other forks), not
   `craft-common-actions`: the upstream rule (CI only through shared workflows) is for upstream;
   the fork's release must not change upstream CI. Upstream `release.yml` is untouched.

## Coverage (2026-10-30)

Corpus (scratch only, never committed): ACadSharp `samples/` (MIT: R12–2018 DWG and DXF, ASCII and
binary, dynamic blocks, AEC objects), ezdxf `examples_dxf/` and `integration_tests/data/` (MIT),
and 13 Autodesk AutoCAD sample drawings (architectural, civil, blocks and tables, plot styles,
visualization; downloaded for testing, not redistributed): **144 files**, plus a generated
1,500,000-entity floor plate (94 MB DXF, 30.6 MB DWG) for memory and liveness. Every file went
through every tool (`open_drawing`, `read_drawing` × 9 sections, `search_drawing`,
`render_preview` model, layouts and 4×4 tiles, PNG, `export` PDF/SVG/DXF/DWG, `roundtrip_check`).

### Versions

| Version | DWG read | DXF read (ASCII / binary) | Corpus files (all steps passed) | Write |
|---|---|---|---|---|
| R12 and earlier (AC1009, AC1006, AC1003) | no (pre-R13 DWG is another format; not supported by the bridge) | yes / yes (binary R12 fixed in this branch) | 9 DXF + 2 binary (all) | — |
| R13 (AC1012) | claimed by the bridge, **untested** (no sample) | yes / yes | 2 + 1 (all) | — |
| R14 (AC1014) | yes | yes / yes | 1 DWG, 1 + 1 DXF (all) | — |
| 2000 (AC1015) | yes | yes / yes | 1 DWG, 5 + 2 DXF (all) | DWG, DXF |
| 2004 (AC1018) | yes | yes / yes | 1 DWG, 2 + 1 DXF (2 DXF fail DWG *export* only: the bridge's writer has no DATATABLE cell type 6) | DWG, DXF |
| 2007 (AC1021) | yes | yes / yes | 8 DWG, 7 + 1 DXF (all) | DWG, DXF |
| 2010 (AC1024) | yes | yes / yes | 7 DWG, 2 + 1 DXF (all) | DWG, DXF |
| 2013 (AC1027) | yes | yes / yes | 1 DWG, 36 + 1 DXF (all) | DWG, DXF |
| 2018 (AC1032) | yes | yes / yes | 16 DWG, 34 + 1 DXF (all) | DWG, DXF |

### Entities and drawing structure

| What | Read (text/values/attributes) | Preview / PDF / SVG | Edit | Corpus count |
|---|---|---|---|---|
| Lines, polylines (2D/3D), arcs, circles, ellipses, splines, points, rays, xlines | yes (type, layer, bounds, geometry) | yes | create, change, delete | 5,317 lines, 922 polylines, 2,329 circles… |
| TEXT, MTEXT (formatting removed), multileader text, table cells | yes: text, layer, space, position, height, rotation, style | yes (TrueType and CADCraft's stroke font; Arabic shaped right to left) | create, set_text, properties | 2,457 TEXT, 476 MTEXT, 338 MLEADER, 47 TABLE |
| Dimensions (linear, aligned, angular, radius, diameter, ordinate, arc length) | yes: shown text (overrides, dual units), measured value, kind, points | yes | create (dimlinear …), override text | 325 |
| Hatches, solids, wipeouts, 3D faces, images, MLINE, tolerances | listed with layer/bounds | yes (images as frames when the file is not staged) | via commands | 221 hatches, 36,142 solids… |
| Blocks and block references, attributes, attribute definitions | yes: every attribute (tag, value), texts inside blocks with the block path and the reference's transform | yes | define, insert (with attribute values), set_attributes (one reference or every reference of a block), rename, delete (references too) | 931 references |
| Title blocks | ranked candidates with all attributes and fixed texts, plus texts drawn on each sheet | yes | set_attributes | found on 29 files (e.g. `title`, 13 attributes, on "SECTIONS AND DETAILS") |
| Layers | yes: on/frozen/locked/plot, colour, linetype, lineweight, entity count | layers_on / layers_off / only_layers; `<g data-layer>` in the SVG | new, set, delete, current, rename | up to 49 per file |
| Layouts / paper space, viewports | yes: paper, viewports with scale and frozen layers | each layout; a viewport's model view | layout commands (new, rename, delete, set) | 237 viewports |
| Xrefs | yes: name, path, overlay, load status; texts read through them | yes when loaded | xattach (paths confined to the roots) | none in the corpus; contract test |
| 3D solids, surfaces, AEC objects (proxies) | counted as Proxy, kept and written back raw | **not drawn** | no | 270 proxies |

### Capability → tool → test

Tests: `apps/cadcraft-cli/tests/mcp_contract.rs` (C:) and the corpus run (K: every file).

| Capability | Tool | Test |
|---|---|---|
| Open DWG / DXF (all versions above) | `open_drawing` | K; C: `save_as_dwg_and_dxf_versions_and_open_them_back` |
| Summary: units, version, extents, layouts, layers, blocks, counts, title blocks | `read_drawing` summary | C: `read_summary_text_dimensions_title_block_and_sections`; K |
| All texts with layer / space / position (inside blocks too) | `read_drawing` text | C: same; K (4,702 texts) |
| Dimension values | `read_drawing` dimensions | C: same; K (325) |
| Title-block attributes | `read_drawing` title_block | C: same; K |
| Entities, layers, blocks, layouts, xrefs | `read_drawing` sections | C: same, `xrefs_attach_list_and_read_through`; K |
| Windowed output (limit / continue_from; markdown) | every list | C: `read_summary…` (paging covers every text once) |
| Search text, attribute values and tags, dimensions, block / layer / layout names | `search_drawing` | C: `search_finds_texts_attributes_dimensions_layers_and_blocks`; K |
| Picture for a visual check | `render_preview` format png | C: `preview_and_export`; K |
| Preview SVG: model, layout, viewport, window, tiles, layers on/off | `render_preview` | C: `preview_and_export`; K (model, 3 layouts, 4×4 tiles each file) |
| Create entities, layers, blocks (batched) | `edit` | C: `edit_and_delete_in_batches_all_or_nothing` (200 ops in one call), `read_summary…` (builds the sheet) |
| Change texts, attributes, properties | `edit` set_text / set_attributes / set_properties | C: `edit_and_delete…` |
| Delete entities, layers, blocks | `edit` delete / layer delete / block delete | C: `edit_and_delete…` |
| All or nothing; cancel rolls back | `edit` atomic, `cancel` | C: `edit_and_delete…`, `jobs_busy_cancel_and_evict` |
| Save DWG 2000–2018, DXF 2000–2018 | `save_drawing` | C: `save_as_dwg_and_dxf_versions…` (2018, 2013, 2000 DWG; 2018, 2007 DXF) |
| Export DWG→DXF, →PDF (all layouts), →SVG, PNG | `export` | C: `preview_and_export`; K (PDF, SVG, DXF all 144; DWG 142/144) |
| What a save loses | `roundtrip_check` | C: `save_as…` (lossless); K (137 lossless; 7 report R2000/2004 multileaders becoming leader + mtext, which those versions require) |
| Roots, OLD_ copies, errors with next_step | all | C: `roots_errors_old_copies_and_handles`, `no_old_copies_flag_replaces_files` |
| Jobs, busy, progress, cancel, evict, recovered files | contract tools | C: `jobs_busy_cancel_and_evict`, `hang_up_with_unsaved_work_leaves_a_recovered_file` |
| Build smoke (Arabic line → DWG, DXF, PDF; read back) | all | `apps/cadcraft-cli/tests/whiq_smoke.py` (release workflow) |

### Size and memory (dev build, Linux; generated floor plate)

| Step | 94 MB DXF, 1.5 M entities | 30.6 MB DWG, same drawing |
|---|---|---|
| open | 13–21 s; 1.5 GB resident after, 3.7 GB peak | 29 s; 3.6 GB after, **7.35 GB peak** (inside the DWG bridge) |
| summary / text page / search | 0.7 s / 1.0 s / 0.3–0.4 s | 0.8 s / — / 0.4 s |
| preview of a window or one tile | 2.2–2.8 s (only the objects in it are built) | 2.8 s |
| PNG of the whole extents | 29 s, 2.5 GB peak | — |
| export DWG / PDF | 28 s / 35 s (4.3 GB peak) | save 27 s, 4.6 GB peak |

Everyday drawings are small by comparison: the largest real sample (36,437 entities, 1.7 MB DWG)
peaks at 292 MB through every tool.

## Known gaps

- Pre-R13 DWG (R12 and earlier) does not open; R13 DWG is untested.
- 3D solids, surfaces and AEC objects are kept and written back but not drawn or measured.
- Arabic (and other shaped right-to-left text) is drawn into PDFs as filled outlines: it prints, but
  no font is embedded for it and it is not selectable. Latin text in a TrueType style is real PDF
  text. The platform's craft smoke (Arabic face embedded in the PDF) does not apply as is;
  `whiq_smoke.py` checks the face is used for the glyphs instead.
- DWG export of a file holding DATATABLE objects with cell type 6 fails in the DWG writer (2 of 144).
- Xrefs and raster images load from the drawing's folder: the platform stages only the files a
  call names, so an xref the user did not upload shows as not found.
- In the SVG, `data-layer` is the layer of the top-level object (a block reference's layer for its
  contents); server-side `layers_off` / `only_layers` are exact for nested objects.
- A PDF holds at most 256 sheets (CADCraft's plotter limit).
- Memory: about 1 KB per entity resident and 2.5–5 KB per entity at the peak of opening a DWG.

## Wiring it into the platform (for the owner; the platform repository was not touched)

- `engines.lock`: an entry like the craft forks', `kind: "craft"`, `repo: "AshrafMMahdy/cadcraft-whiq"`,
  `bin` `cadcraft-cli` / `cadcraft-cli.exe`, assets `cadcraft-cli-{tag}-linux-x86_64.tar.gz` and
  `cadcraft-cli-{tag}-windows-x86_64.zip` (pin with `scripts/engines-pin.mjs cadcraft@whiq-v0.1.0`
  once `cadcraft` is in its `DEFAULTS` and in `ENGINE_NAMES`).
- The supervisor's flags work as for the forks: `mcp --automation-read-root … --automation-write-root …
  --author … --busy-after-ms 300 [--automation-no-old-copies]`; `server_status.limits.old_copies`
  says which; tools carry `_meta["whiq/files"]` (`open_drawing.path`, `read_drawing.path`,
  `search_drawing.path`, `render_preview.path`, `roundtrip_check.path`, `export.source`).
- Build smoke (`engines-fetch.mjs renderPlan`, extension `dwg`): `new_drawing {}` → `edit {ops:
  [{op: "command", command: "style", params: {name: "ARABIC", font: <fonts.arabic>}}, {op: "command",
  command: "style.current", params: {name: "ARABIC"}}, {op: "add", type: "text", at: [0, 0], text:
  SMOKE_ARABIC, height: 5}]}` → `save_drawing {path: "smoke.dwg"}` → `export {path: "smoke.pdf"}`.
  The document starts `AC10` (not `PK`). The PDF check must not require the Arabic face embedded
  (see Known gaps); `whiq_smoke.py` shows the check that holds (filled glyph paths in
  `render_preview`, the text read back from the DWG).
- The Preview tab: `render_preview` answers SVG tiles (`tiles[].svg`, `window`, y running down);
  `max_bytes` sets how many fit in one answer; each primitive group is `<g data-layer="…">`.
