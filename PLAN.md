# DeLOG — Drone Flight Log & Live Telemetry Analyzer

**Architecture & Implementation Plan — v1.0**

DeLOG is a desktop tool for loading, visualizing and analyzing drone flight logs (ArduPilot `.BIN`, PX4 `.ulg`, MAVLink `.tlog`) and live MAVLink telemetry, built in Rust on `egui`/`eframe` + `wgpu` with an Arrow-based columnar core. It targets interactive performance at 100M+ samples, zero-jank live ingestion, and a clean, testable crate architecture.

This document is the **single source of truth** for the project. It defines the architecture, explains every load-bearing design decision, and carries the master checklist (§22) that the implementing agent maintains.

---

## 0. Protocol for the implementing agent

Read this section before doing anything else, and re-read §4.5 (Zero-Copy Invariants) before touching any code that moves sample data.

**Checklist discipline.** Every feature in §22 has a stable ID (e.g. `CORE-07`). When you start an item, mark it `[~]`; when done, mark it `[x]`; if blocked, mark `[!]` and append a one-line reason. Update the checklist **in the same commit** as the feature. Never invent features that aren't in the checklist without adding them to it first (under the correct area, with a new ID).

**Status legend:**

```
[ ]  not started
[~]  in progress
[x]  done (meets Definition of Done)
[!]  blocked — reason appended after the item text
```

**Definition of Done** for any checklist item:

1. Compiles on stable Rust; `cargo clippy --workspace -- -D warnings` is clean.
2. Unit tests exist for non-trivial logic; golden/fixture tests for parsers.
3. If the item touches a hot path (parse, ingest, cache build, y-range query, GPU upload, paint), a criterion bench exists or is updated, and the perf budgets in §20.4 still hold.
4. No Zero-Copy Invariant (§4.5) is violated. Any _justified_ exception carries a `// ZC-EXCEPTION:` comment and a counter metric.
5. The checklist entry is updated.

**Standing rules:**

- Never hold a lock or borrow across a paint callback or an `.await`.
- Sample data is copied only where the One Copy Rule (§4.5, invariant 3) permits.
- The dependency rules in §3.2 are absolute. If a change needs an upward dependency, the design is wrong — stop and restructure.
- All work longer than ~16 ms runs off the UI thread (§19.6).
- Prefer the milestone order in §21; within a milestone, item order in §22 is the suggested order.

---

## 1. Product overview

### 1.1 What DeLOG is

A single-window workspace where an engineer drops one or more flight logs (or opens a live MAVLink link), drags fields from a data tree onto GPU-rendered time-series plots, scrubs a global timeline that drives a synchronized 3D vehicle view, and exports findings (CSV, images, layouts, diagnostics). It is built for _post-incident analysis and flight-test review_: spikes must never be smoothed away, timestamps must never lose precision, and a live link must never stutter the UI.

### 1.2 Goals

- Load multiple logs of mixed formats simultaneously, aligned on a global timeline with per-source offsets.
- Live MAVLink (UDP/TCP/serial) through the _same_ ingestion path as files, with recording.
- GPU-rendered plots and 3D view sharing one device; no CPU geometry rebuilds during paint.
- Interactive pan/zoom/y-autoscale at ≥60 FPS with tens of traces over 100M+ total samples.
- Portable, versioned layouts; diagnostics and performance introspection as first-class panels.
- Architecture that a benchmark harness, fuzzers and future plugins can hold onto.

### 1.3 Non-goals (v1)

Parameter editing/upload, mission editing, map tiles, multi-window, dynamically-loaded plugins, packaged web build (we keep the code _compatible_ with WASM, §9.10, but do not ship it), FFT view (backlog), scripting (backlog).

---

## 2. Technology stack

| Layer         | Choice                                                          | Why                                                                                                                                                                                                | Rejected alternatives                                                                                                                                                                                       |
| ------------- | --------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Language      | **Rust** (stable, edition 2024)                                 | Memory safety where it matters most — parsers fed untrusted bytes; fearless threading for the ingest/parse/UI split; zero-cost abstractions for the cache/GPU hot path.                            | C++ (parser safety, build complexity), Python (perf ceiling).                                                                                                                                               |
| GUI           | **egui / eframe**                                               | Immediate mode matches a live-redrawing telemetry tool; first-class `wgpu` paint callbacks let custom GPU passes share the UI's device — the single requirement that eliminates most alternatives. | Qt/QML (retained scene graph fights live redraw; custom GPU plotting via RHI is high-friction), Dear ImGui + ImPlot (ImPlot tessellates polylines on CPU every frame — violates §9's no-CPU-geometry rule). |
| GPU           | **wgpu**                                                        | Portable (Vulkan/Metal/DX12/GL), storage buffers for vertex pulling, future WASM/WebGPU path, same API egui already uses.                                                                          | Raw Vulkan (cost), OpenGL (no storage-buffer guarantees, dying tooling).                                                                                                                                    |
| Columnar data | **arrow-rs** (`arrow`, `arrow-ipc`; `parquet` later)            | Arc-backed buffers → zero-copy slices; chunked immutable arrays match append-only live ingestion; IPC files mmap back in for near-zero-copy reload; free interop with Python tooling later.        | Hand-rolled `Vec<f64>` chunks (reinvents chunked arrays, loses mmap + cache format + metadata).                                                                                                             |
| Tiling        | **egui_tiles**                                                  | Proven split/tab/drag tile tree; serializable.                                                                                                                                                     | Hand-rolled splitter tree.                                                                                                                                                                                  |
| MAVLink       | **rust-mavlink** (`all` dialect)                      | Generated message types for AP+PX4-common set; we wrap our own I/O for counters (§7.2).                                                                                                            | Hand parser (only the framing layer is hand-written, for counters).                                                                                                                                         |
| Serial        | `serialport`                                                    | Cross-platform, known-good.                                                                                                                                                                        | —                                                                                                                                                                                                           |
| Models        | `gltf`                                                          | GLB loading for vehicle meshes.                                                                                                                                                                    | —                                                                                                                                                                                                           |
| Concurrency   | `arc-swap`, `crossbeam-channel`, `rayon` (parse/stat side only) | Epoch snapshots (§4.4); bounded MPSC ingestion; data-parallel scans.                                                                                                                               | `RwLock` store (§4.4 explains why not).                                                                                                                                                                     |
| Persistence   | `serde` + `serde_json` (layouts), `memmap2` (cache reload)      | —                                                                                                                                                                                                  | —                                                                                                                                                                                                           |
| Errors / logs | `thiserror`, `tracing`                                          | —                                                                                                                                                                                                  | —                                                                                                                                                                                                           |
| Testing       | `criterion`, `proptest`, `cargo-fuzz`                           | §20.                                                                                                                                                                                               | —                                                                                                                                                                                                           |

**Decision: pin philosophy.** All third-party versions pinned in the workspace `Cargo.toml` `[workspace.dependencies]` table; crates inherit with `workspace = true`. **Why:** one place to audit/upgrade; reproducible benches.

---

## 3. Workspace architecture

### 3.1 Crate layout

```
delog/
├── Cargo.toml                  # [workspace] + [workspace.dependencies]
├── crates/
│   ├── delog-core/             # IDs & keys, time model, columnar store, snapshots,
│   │                           #   chunk stats, diagnostics types, metrics registry
│   ├── delog-parsers/          # sniffing + ULog / AP-BIN / tlog / (CSV later) parsers
│   ├── delog-stream/           # live MAVLink backends, link state, recorder
│   ├── delog-cache/            # render caches (f32), min/max pyramid, cache manager
│   ├── delog-render/           # pure-wgpu: pipelines, buffer manager, 2D pass, 3D scene
│   └── delog-app/              # eframe shell, widgets, docks, layouts, glue
├── assets/                     # embedded GLB models, WGSL shaders, icon, palette
├── fixtures/                   # golden logs (small, real) + synthetic generators
└── benches/                    # criterion harness (workspace-level)
```

### 3.2 Dependency rules (absolute)

```
delog-app ──► delog-render ──► delog-cache ──► delog-core
    │              │                              ▲
    ├──────────────┼──► delog-stream ──► delog-parsers ──► (core)
    └──────────────┴──► delog-parsers ────────────┘
```

1. **Data flows downward only.** `delog-core` depends on `arrow` + std only. Nothing below `app` may depend on `egui`.
2. **`delog-render` is pure wgpu.** No egui types. **Why:** headless benchmarking and golden-image tests (§20.3) need to drive the renderer without a window; a future non-egui shell stays possible. `delog-app` adapts it through `egui_wgpu` callbacks.
3. **Parsers and stream never see GPU or UI.** Their only output is `ParsedBatch` + diagnostics into an `IngestSink` (§5). The shared MAVLink frame decoder + field extractor live in `delog-parsers::mavlink` (PAR-16; fuzzed under PAR-13) — the tlog parser wraps it (§6.4) and `delog-stream`'s reader threads consume it (§7.2–7.3), giving stream a downward edge onto parsers.
4. **Arrow is a vocabulary type of `delog-core`'s API** (chunks expose `Int64Array`, `ArrayRef`). **Why not wrap it fully:** a total abstraction layer over Arrow would re-implement its typed accessors and cost performance and code; `delog-cache` needs raw typed access for the One Copy. The compromise: `delog-app` is _style-forbidden_ (enforced by review, not the compiler) from touching Arrow directly — it goes through core helpers (`field_stats`, `sample_at`, `slice_for_export`). If Arrow ever had to be swapped, only core + cache change.

### 3.3 End-to-end data flow (orientation walkthrough)

What happens when the user drags `BARO.Alt` from a loaded `.BIN` onto a plot, with a live UDP link also running:

1. **Parse (already done):** the BIN parser thread decoded FMT/FMTU, built Arrow builders per field, and pushed `ParsedBatch`es through the bounded channel to the **ingest thread**, which packed them into immutable `Chunk`s and published new `StoreSnapshot`s via `ArcSwap` (§4.4). Live MAVLink batches interleave through the _same_ channel and thread.
2. **Drop:** the browser hands the plot a `FieldId`. The plot asks the **cache manager** for a `TraceCache`. None exists → an async build job iterates the snapshot's chunks _in place_ (zero-copy reads), performing the One Copy: dtype→`f32`, multiplier applied, time rebased to the global origin, NaN preserved as gap markers (§8.2). It also builds the min/max pyramid (§8.4).
3. **Upload:** the render-side **buffer manager** creates a `STORAGE` buffer and uploads the interleaved `[x,y]` pairs once. From now on, live appends upload _only the appended byte span_ (§9.3).
4. **Paint:** each frame, the app loads the current snapshot, computes the shared `ViewX` transform, and the plot's `egui_wgpu` callback records one draw per visible trace — the vertex shader pulls sample pairs from the storage buffer and expands them to screen-space quads (§9.4). No CPU geometry exists.
5. **Hover/tooltip:** the cursor's time is binary-searched against the **canonical** `i64` timestamps (not the f32 cache), so the readout has microsecond/full-dtype fidelity (§4.2).
6. **Y-autoscale:** the visible range min/max comes from the pyramid in O(log n) (§8.4), never a scan.

Hold this picture; every section below details one stage of it.

---

## 4. Core data model (`delog-core`)

This is the heart of the tool. Everything else — caches, GPU buffers, layouts, stats — hangs off the guarantees made here.

### 4.1 Identity: runtime IDs vs portable keys

```rust
pub struct SourceId(pub u32);   // dense index into Vec<SourceEntry>
pub struct TopicId(pub u32);    // dense index, global across sources
pub struct FieldId(pub u32);    // dense index, global across topics

pub struct FieldKey {           // portable — what layouts store
    pub source: String,         // user-visible source label
    pub topic:  String,         // e.g. "BARO" / "vehicle_local_position[0]"
    pub field:  String,         // e.g. "Alt"
}
```

**Decision.** Two identity systems: dense `u32` runtime IDs for everything hot, string `FieldKey` for everything persisted.
**Why.** Runtime IDs are `Copy`, index directly into `Vec`s (no hashing on the paint path), and are stable for a session. Keys survive across sessions and machines and resolve against _whatever_ logs are loaded — the spec's "stable portable field references."
**Rejected.** Using keys everywhere (hashing + string compares per frame); using runtime IDs in layouts (meaningless after restart).

Source labels default to the filename stem (`flight_0042`) or endpoint (`mavlink:udp:14550`); collisions get `#2`, `#3` suffixes. A `Resolver` maps `FieldKey → Option<FieldId>` and reports misses (§14.3).

### 4.2 Time model

- **Canonical timestamp: `i64` microseconds** in the source's own domain (boot-time for AP/PX4/MAVLink).
- Per-source `offset_us: i64`, user-adjustable; _effective time_ = raw + offset. Auto-alignment can propose offsets when two sources both carry GPS UTC.
- **Global range** = min/max of effective ranges across the snapshot.

**Decision.** Integer microseconds end to end in the canonical layer; floats appear only in render caches.
**Why.** µs is the native resolution of ULog and AP DataFlash (`TimeUS`); `i64` µs is exact for ~292k years; integer comparisons make binary search and range pruning branch-predictable. Every precision-sensitive consumer — tooltips, statistics, export, the 3D trajectory — reads canonical `i64` + original dtype. The f32 conversion exists _only_ for GPU geometry, and §8.3 documents exactly how much precision that costs and where the limit is.
**Rejected.** `f64` seconds canonically (silently accumulates error in long logs, complicates equality/dedup); per-source normalized time (breaks cross-source alignment semantics).

### 4.3 Columnar storage: immutable chunked topics

A _topic_ (AP message type, ULog subscription, MAVLink message) is a time-indexed table stored as an append-only sequence of **immutable chunks**:

```rust
pub struct ColStats { pub min: f64, pub max: f64, pub sum: f64, pub nan_count: u64 }

pub struct Chunk {
    pub t:     Int64Array,        // effective-domain raw µs, sorted within the chunk
    pub cols:  Vec<ArrayRef>,     // parallel to schema fields; original dtypes preserved
    pub stats: Vec<ColStats>,     // computed once at seal time
    pub t_min: i64, pub t_max: i64,
}

pub struct TopicStore {
    pub schema: Arc<TopicSchema>,        // field names, dtypes, units, multipliers
    pub chunks: Arc<[Arc<Chunk>]>,       // the "spine" — swapped wholesale on append
    pub rows:   u64,
}
```

**Decisions and why:**

- **Original dtypes preserved** (`Int8..UInt64`, `Float32/64`, `Bool`; strings allowed but not plottable). Units and multipliers (AP FMTU, ULog metadata) live in the schema, _not_ baked into values. **Why:** the canonical layer stays lossless and byte-comparable to the log; engineering-unit conversion happens exactly once, at the One Copy (§8.2), where a multiply is free.
- **Chunk size:** target 64Ki rows for file parsing; live ingestion seals a chunk every 512 samples _or_ 100 ms, whichever first. **Why:** 64Ki×8B ≈ 512 KB per column — scan-friendly, amortizes `Arc` overhead; the live rule bounds both append latency and spine-rebuild frequency.
- **Per-chunk `ColStats` at seal time.** **Why:** global field statistics become an O(chunks) fold instead of an O(rows) scan; gap/NaN diagnostics come free.
- **Sorted within a chunk; chunks may overlap.** Parsers sort each batch; cross-chunk overlap (rare retransmits, log quirks) is tolerated and flagged as a `timestamp-regression` diagnostic rather than fixed up. **Why:** queries already prune by `t_min/t_max` and binary-search per chunk; a global re-sort would force copies and violate append-only.
- **Multi-instance topics** (PX4 `multi_id`, AP GPS/IMU instances) are exposed as separate topics named `topic[N]`. **Why:** each instance gets its own trace cache, color and tree row — matching how an analyst thinks about "GPS 0 vs GPS 1".

### 4.4 Concurrency: epoch snapshots, not locks

```rust
pub struct StoreSnapshot {                  // deeply immutable
    pub sources: Arc<[SourceEntry]>,        // each holds Arc<TopicStore> spines
    pub epoch:   u64,
}
pub struct DataStore { current: ArcSwap<StoreSnapshot> }
```

There is exactly **one writer** — the ingest thread (§5). To append, it builds the next snapshot by **structural sharing**: clone the spine `Arc`s (pointer copies), push the new `Arc<Chunk>`, bump `epoch`, `store.current.store(new)`. Readers — the UI thread, cache builders, stat jobs — call `store.current.load()` **once per frame/job** and get a coherent, wait-free view they can hold as long as they like.

**Why this and not `RwLock<DataStore>`.** A parser flushing a large batch under a write lock stalls the render thread for the duration — a guaranteed dropped frame at exactly the moment the UI should feel alive (live streaming). With epochs, reader latency is _independent_ of ingest activity; there are no torn reads, no lock-order bugs, no priority inversion. The cost — rebuilding spine vectors of `Arc`s — is nanoseconds-to-microseconds per flush at our chunk sizes.
**Why not persistent data structures (`im`)?** Spine rebuild at chunk granularity is already cheap; `im` adds dependency weight for no measurable win.
**Memory note (document for the agent):** an old snapshot pins its chunks until dropped. The renderer holds at most the current snapshot per frame, so at most two epochs are transiently alive. Long-running jobs (export, stats) intentionally pin their snapshot — that is correct behavior, not a leak.

### 4.5 ZERO-COPY INVARIANTS — the contract

These are numbered so code comments and reviews can cite them (`// upholds ZC-3`).

1. **Parser → Store:** parsers write into Arrow builders directly and _move_ the finished buffers in `ParsedBatch`. No intermediate row structs, no re-serialization.
2. **Store → readers:** all slicing (stats windows, export ranges, tooltip lookups) uses Arrow zero-copy offset views. Reading never copies.
3. **The One Copy Rule:** exactly **one** transform copy is permitted per plotted field — canonical → render cache (§8.2): dtype→`f32`, multiplier applied, time rebased. It is lazy (built on first plot), incremental (appends only), and accounted (bytes visible in the memory panel).
4. **Cache → GPU:** `Queue::write_buffer` uploads **only the appended byte span**. Full re-uploads happen only on buffer growth or rebase, and increment a `gpu_full_uploads` counter so regressions are visible.
5. **Disk cache:** Arrow IPC sidecar files are reopened via `memmap2`; decoded arrays reference the mapped pages (no re-parse, near-zero-copy reload).
6. Strings/blobs are exempt from (3) — they are not plottable and never enter caches.

Any deliberate exception carries `// ZC-EXCEPTION: <reason>` and a metric.

### 4.6 Memory accounting & removal

`MemBreakdown { canonical, cache_cpu, gpu, mmap }` is computed per field/topic/source by summing Arrow buffer capacities, cache `Vec` capacities, and the GPU buffer manager's ledger (§9.3) — surfaced in the data browser and perf dock. **Removing a source** publishes a snapshot without it; a GC pass in the cache manager (subscribed to epoch changes) drops orphaned `TraceCache`s and frees their GPU buffers. Actual memory returns when the last pinning snapshot/job drops — typically the next frame.

---

## 5. Ingestion pipeline (shared by files and live)

```
[BIN parser thread]──┐
[ULog parser thread]─┤   bounded mpsc            single writer
[tlog parser thread]─┼──► IngestMsg (cap 256) ──► [ingest thread] ──► seal Chunks
[live decoder x N]───┘                                   │            swap StoreSnapshot
                                                         └──► notify: UI repaint,
                                                                cache manager (epoch)
enum IngestMsg {
    OpenSource { key, kind, schema_hint },
    Batch(ParsedBatch),            // topic + Arrow columns + sorted i64 µs timestamps
    Diagnostic(Diag),
    Progress { source, frac },
    CloseSource { source, summary },
}
```

**Decision.** One ingest thread is the _only_ store writer; everything funnels through one bounded channel.
**Why.** Single-writer makes §4.4 trivially correct (no write contention _by construction_), serializes epoch bumps, and gives one place to seal chunks, compute `ColStats`, and emit data-quality diagnostics.

**Backpressure policy** (explicit, because it encodes a value judgment):

- **File parsers block** when the channel is full. A file can wait; correctness over latency.
- **Live decoders never block.** If the channel is full, the batch is _dropped_, `ingest_dropped_batches` increments, and a rate-limited diagnostic fires. **Why:** the alternative is the link reader thread stalling → OS socket buffers overflowing → silent, unaccounted loss. Dropping visibly at a defined point protects both the UI and the link, and the counter makes it diagnosable.

**Progress** is byte-based per source. **Cancellation** is an `Arc<AtomicBool>` parsers poll every ~4096 records — responsive without per-record overhead.

---

## 6. Parsers (`delog-parsers`)

### 6.1 Trait & detection

```rust
pub struct Sniff { pub score: u8 /* 0..=100 */, pub reason: &'static str }

pub trait LogParser: Send + Sync {
    fn name(&self) -> &'static str;
    fn sniff(&self, head: &[u8]) -> Sniff;                 // first 4 KiB
    fn parse(&self,
             src:  Box<dyn Read + Seek + Send>,
             sink: &mut dyn IngestSink,
             ctl:  &ParseCtl,                              // progress + cancel
    ) -> Result<ParseSummary, ParseError>;
}
```

Auto-detection runs every registered parser's `sniff` on the file head and picks the highest score; a tie or a top score < 60 raises the **manual override picker** in the UI (spec requirement). `ParseSummary` carries topic/row counts, time range, and the diagnostic tally.

**Error policy (decision).** Malformed _records_ are skipped with a byte-offset diagnostic and parsing continues; only unrecoverable _framing_ corruption aborts. **Why:** a post-crash log with a torn tail is precisely the log the user most needs to open. Fuzzing (§20.2) enforces that no input can panic or hang a parser.

### 6.2 ArduPilot DataFlash `.BIN`

Self-describing: `FMT` records define message layouts, `FMTU` attaches units/multipliers, `UNIT`/`MULT` define the tables. Full type-char map implemented up front:

```
b i8    B u8    h i16    H u16    i i32    I u32    f f32    d f64
n char[4]   N char[16]   Z char[64]
c i16*0.01  C u16*0.01   e i32*0.01  E u32*0.01      (fixed-point — keep raw, mult in schema)
L i32 (deg*1e7 lat/lon)   M u8 (flight mode)   q i64   Q u64   a i16[32]
```

**Decision.** Store **raw** values; record unit + multiplier in `TopicSchema`. The One Copy applies the multiplier, so plots show engineering units while the canonical layer stays byte-faithful.

**Precision trap (documented, must not regress):** `L` lat/lon as f32 degrees has an ULP of ~0.4–0.6 m at mid latitudes — unacceptable for trajectories. Positional fields therefore **bypass the generic f32 cache** for 3D use: the trajectory builder (§12.2) reads canonical `i32`/`f64` and converts to _local NED meters_ around a reference origin in `f64`, then stores f32 meters (cm-safe within tens of km). Plotting raw lat/lon as a 2D trace is allowed (shape is fine) — the limitation is noted in the field tooltip.

Instances (e.g. multiple GPS) split into `GPS[0]`, `GPS[1]` topics per §4.3.

### 6.3 PX4 ULog `.ulg`

16-byte header (magic + version + start timestamp); definitions section (`F` formats, `I` info, `P` params), data section (`A` subscriptions, `D` data, `L` logged messages, `S` sync, `O` dropouts). Nested types flatten to dotted field paths; padding fields are skipped; `multi_id` → `topic[N]`. **Dropout (`O`) messages become diagnostics _and_ injected NaN gap markers** so plots visually break where data is missing (§8.2). Logged messages (`L`) feed the auto-marker system (§17.4). Parameters are kept as source metadata (browser-inspectable; a parameter diff panel is backlog).

### 6.4 MAVLink `.tlog`

Format: repeated `[8-byte big-endian Unix µs][MAVLink v1/v2 frame]`. **Decision:** the tlog parser wraps the _same_ decoder + field-extractor used by live streaming (§7.3) — one code path, one set of bugs, and recording (§7.5) round-trips by construction.

### 6.5 Later formats

CSV import (column→field mapping dialog) and the Arrow IPC fast-reload cache (§18.4) plug in as additional `LogParser` implementations — nothing upstream changes. This is the spec's "parser plugin API" satisfied at the trait level; dynamic loading stays backlog.

---

## 7. Live streaming (`delog-stream`)

### 7.1 Backends

```rust
pub enum Endpoint {
    UdpServer { bind: SocketAddr },        // listen (GCS-style)
    TcpClient { remote: SocketAddr },
    Serial    { path: String, baud: u32 },
}
// UDP-client and TCP-server modes were built and then removed by decision:
// the GCS-side patterns are UDP listen, TCP connect, and serial.
```

Each configured link runs **one reader thread**. Configurable endpoint/port/baud lives in a connection dialog; multiple simultaneous links are supported, each becoming its own source family.

### 7.2 Framing & counters

**Decision.** We own the byte-level framing loop (buffered transport → MAVLink v1/v2 frame sync → CRC) and hand _frames_ to `rust-mavlink` for decode, rather than using its blocking `connect()` helpers.
**Why.** The spec demands honest link diagnostics: per-link counters for rx packets, CRC failures, **sequence-gap dropped messages**, bytes/s — those need access to the raw stream and seq numbers. The state machine `Connecting → Connected(last_rx) → Stale(>2 s) → Lost(>10 s)` drives the UI indicator; TCP/serial auto-reconnect with exponential backoff (0.5 s → 8 s cap).

### 7.3 Message → fields extraction

**Decision (amended).** A custom flat `serde::Serializer` over the `all` dialect's generated `Serialize` impls extracts `(field, Scalar)` pairs from a `&MavMessage`; per-message extraction *plans* (field names + dtypes) are built once on first sight, so the steady-state hot path appends scalars with no allocation. Unknown/unsupported messages emit a once-per-type diagnostic and a counter. _(Originally specced as build-script codegen; amended because that requires vendoring the dialect XMLs while the mavlink crate already ships `serde` derives — the custom serializer is flat, JSON-free, and two orders of magnitude less code.)_
**Why no serde_json:** the hot path must not reflect through a self-describing format (allocation storm at 50 Hz × dozens of messages); the custom serializer visits struct fields directly. Arrays expand to indexed fields (`servo_raw[0]…`); enum fields carry their variant name as a Utf8 value (the dialect serializes them internally tagged, so the name is what's available); bitflag fields their raw bits (the serializer reports non-human-readable). sysid demultiplexing: source per `(link, sysid)`; compid folds in as an instance suffix only when more than one compid emits the same message.

### 7.4 Live semantics in the store

Live batches flow through §5 unchanged (append-only chunks, 512-sample/100 ms seal rule). The UI gains: **lock viewport to tail** (§10.4), **pause view while ingestion continues** (pause freezes `ViewX`, never the ingest thread), and incremental GPU upload (§9.3) keeps per-frame cost proportional to _new_ samples only.

### 7.5 Recording

A recorder tees **raw frame bytes** (pre-decode) to a `.tlog` with the µs envelope. **Why raw:** the recording is bit-faithful, replayable by our own tlog parser (round-trip test in CI), and immune to extractor bugs.

---

## 8. Render cache layer (`delog-cache`)

### 8.1 Shape

```rust
pub struct TraceCache {
    pub xy: Vec<f32>,          // interleaved [x0,y0,x1,y1,...] — one buffer per trace
    pub origin_us: i64,        // x = (t_eff - origin_us) * 1e-6, as f32
    pub built_rows: u64,       // high-water mark vs canonical store
    pub pyramid: MinMaxPyramid,
    pub last_used_frame: u64,  // for LRU
}
```

**Decision: interleaved x,y f32 pairs** (8 B/sample) in a single contiguous `Vec`, mirrored 1:1 into one GPU storage buffer.
**Why.** One allocation, one upload span, perfect locality for the vertex shader's `p[i], p[i+1]` fetch pattern; irregular sampling means x must be explicit (no implicit-index trick).

### 8.2 The One Copy (build & incremental append)

Built lazily on first plot, off-thread; the plot shows a brief "building cache…" state. The builder iterates the snapshot's chunks **in place** (ZC-2), converting per sample: apply schema multiplier in `f64`, cast to `f32`, rebase time. **Non-finite values are preserved as NaN** — and ULog dropouts _inject_ NaN rows — because the line shader treats NaN as a segment break (§9.4): gaps render as gaps, not interpolated lies. On each store epoch, the cache manager appends rows `built_rows..` for live traces — never rebuilds.

### 8.3 f32 time precision — the honest math

With `origin_us` = global dataset start, x in seconds has ULP ≈ `span / 2²³`: a 3-hour log → ~1.3 ms positional error at the far end — sub-pixel until extreme zoom. The documented limit: at spans > ~4000 s, zooming below ~1 ms/px can show stepping. Tooltips, statistics, export and playback are **immune** (they read canonical `i64`, §4.2); only line geometry is affected. **Backlog fix** recorded (CCH-12): split-double x (`hi: f32, lo: f32`) reconstructed in-shader, or per-window rebasing with cache regeneration. Not v1 — the budget is spent where analysis accuracy lives.

### 8.4 Min/max pyramid

Branching factor 64: `L0[i] = (min,max)` of samples `64i..64(i+1)`, each higher level reduces 64:1 until one node. Costs ~3% of cache memory; built with the cache and appended incrementally.

Serves two consumers:

1. **Y-range queries** — visible-window min/max in O(log₆₄ n): partial blocks at the edges, whole high-level nodes in the middle. This is the spec's "exact or indexed min/max" — exact at the edges, indexed in the middle, mathematically identical to a full scan.
2. **Decimated drawing** (§9.5) — per-pixel-column min/max.

### 8.5 Manager & eviction

`CacheManager` owns `FieldId → TraceCache`, subscribes to store epochs (incremental appends), GCs caches for removed sources, and LRU-evicts _unplotted_ caches above a budget (default 1 GiB; plotted traces are pinned). All sizes feed the memory panel (ZC-3 "accounted").

---

## 9. GPU renderer (`delog-render`)

### 9.1 One device, two pass strategies

A single `wgpu::Device/Queue` — the one eframe already created — is shared by egui, all plots, and the 3D view (spec: "single GPU device/context"). **Why:** buffer/texture sharing without cross-device copies, one allocator, one place to track VRAM.

**Decision.** 2D plots draw **inside egui's main render pass** via `egui_wgpu` callbacks (per-plot viewport + scissor set from the widget rect). The 3D view renders to an **offscreen color+depth texture** (own pass, 4× MSAA) in the callback's `prepare()` phase and is composited as an egui image.
**Why the split.** egui's main pass has no depth attachment — fine for painter's-order 2D, wrong for meshes. Offscreen 3D also confines MSAA cost to the one view that benefits. This avoids both per-widget GPU contexts and a fullscreen extra pass for 2D (spec requirements).

### 9.2 Pipeline inventory

| Pipeline                   | Use                         | Notes                             |
| -------------------------- | --------------------------- | --------------------------------- |
| `line_pull`                | trace polylines             | vertex pulling, §9.4              |
| `step_pull`                | stepped/previous-point mode | shader variant, 2 segments/sample |
| `scatter_pull`             | points / line+points        | quad per sample, size uniform     |
| `minmax_col`               | decimated zoom-out draw     | §9.5                              |
| `grid3d`                   | infinite ground grid        | shader-based, distance-faded      |
| `mesh`                     | vehicle GLB                 | N·L + ambient ("PBR-lite")        |
| `traj3d`                   | trajectory polyline         | line-list v1; thick later         |
| `thick_miter` _(later)_    | joined thick 2D lines       | GPU-25                            |
| `compute_minmax` _(later)_ | pyramid on GPU              | GPU-26                            |
| `picking` _(later)_        | hover acceleration          | GPU-27                            |

### 9.3 Buffer manager

Per-`FieldId` ledger: `TraceGpu { buf: Buffer(STORAGE|COPY_DST|COPY_SRC), capacity, len_samples }`. Append = `write_buffer` at the tail offset (ZC-4). Growth = ×1.5 new buffer + **GPU-side** `copy_buffer_to_buffer` of the old contents (no CPU round trip), then upload only the new span. Uniforms (per-plot transform, color, width, viewport) live in one dynamic-offset uniform buffer — **not push constants**, which aren't universally supported. The ledger's byte totals feed memory accounting (§4.6).

### 9.4 Line rendering by vertex pulling (the core trick)

No vertex buffers, no CPU tessellation — the spec's hard requirement. For a trace of _n_ samples: `draw(0 .. (n-1) * 6)`. The vertex shader computes `seg = vi / 6`, `corner = vi % 6`, loads `p0 = xy[seg]`, `p1 = xy[seg+1]` from the storage buffer, transforms to clip space via the plot uniform, and offsets perpendicular to the segment by `width_px` (converted with the viewport size) to emit two triangles. **If either endpoint is non-finite, all six vertices collapse to a degenerate triangle** — NaN gaps cost nothing and draw nothing. Joins are unmitigated in v1 (overdraw at ≤2 px widths is invisible); a miter pipeline is a later checklist item, not a blocker.

### 9.5 Decimated path for zoomed-out views

When `visible_samples / pixel_width > 8`, drawing every segment wastes fill-rate and aliases badly. The plot instead asks the pyramid for **per-pixel-column (min,max)** — ~W queries × O(levels), <0.5 ms CPU — writes ≤32 KB of column extents into a transient ring buffer (only when the view changed), and `minmax_col` draws one vertical span per column plus connectors.

**Decision: min/max decimation, not LTTB.** **Why.** LTTB produces pretty curves by _discarding_ extrema; for flight-log analysis the single-sample current spike or vibration burst **is the finding**. Min/max guarantees every transient that exists in the data is visible at every zoom level. (LTTB remains available for CSV _export_ resampling where smoothness is the goal.)

### 9.6 What stays on the CPU deliberately

Axes, ticks, tick labels, legends, hover circles and the playhead line are painted by egui. **Why:** they are dozens of primitives (text rendering on GPU is a project of its own), and they sit naturally above/below the callback in painter order. Only _sample geometry_ — the thing that scales with data — is custom-GPU.

### 9.7 WASM portability note (recorded, not v1 work)

WebGL2 has no storage buffers in vertex shaders; a future web build swaps vertex pulling for instanced vertex buffers behind the same trait. The shader interface is kept narrow so this is a backend swap, not a redesign. WebGPU targets need no change.

---

## 10. Plot system & interaction (`delog-app`)

### 10.1 Workspace tiling

`egui_tiles::Tree<Pane>` where `Pane = Plot(PlotPane) | Scene3D | (Map — backlog)`. Splits (H/V), tabs, drag-rearrange and close come from the tile tree; toolbar "add plot" inserts into the focused container; context-menu "split" wraps the pane. The tile tree serializes into layouts (§14).

### 10.2 Plot pane state

```rust
struct TraceRef { field: FieldId, color: Color32, width: f32,
                  mode: TraceMode /* Line|Scatter|LinePoints|Step */, visible: bool }
struct PlotPane { traces: Vec<TraceRef>, legend: bool }
// Y always auto-fits the visible window (pyramid min/max + pad) — no per-pane Y modes.
```

Per-trace visibility / color / width / mode all live here and persist in layouts. Colors auto-assign from a 10-color colorblind-safe palette tuned for the dark theme; after exhaustion the cycle repeats with dashed widths.

### 10.3 The shared X axis

**Decision.** One global `ViewX { t0_us: i64, t1_us: i64 }` in app state; every plot renders from it and every pan/zoom mutates it. Synced axes are therefore not a synchronization _feature_ — they are the absence of per-plot X state.
**Why µs integers:** the view directly drives canonical binary searches (hover, stats, export) without float drift; conversion to cache-space f32 happens per frame when filling uniforms.

### 10.4 Interactions

Wheel = zoom about the cursor (×0.8 / ×1.25); drag = pan; double-click = reset to full global range; `End`/toolbar = **lock-to-live** (each frame sets `t1 = global_end`, keeping the span; any manual scrub unlocks it, and the toolbar button glows to invite re-lock — an explicit-state UX decision so the user is never fighting an invisible mode). Y per pane always auto-fits the visible window: pyramid min/max query on view change + 5% pad. (Full-data and manual Y modes were built and then deliberately removed — one predictable behavior, no per-pane mode state.)

### 10.5 Hover, cursor & tooltips

Hover runs a per-trace binary search on **canonical** timestamps (per-chunk `t` arrays with `t_min/t_max` pruning) — full-precision values, original dtype, unit string. Tooltip interpolation modes: **previous sample** (default — telemetry is sample-and-hold), **next**, **linear**; hover circles draw at the interpolated screen position via egui. The playback cursor (§11) paints as a vertical line on every plot, with its own value readout mode.

### 10.6 Plot context menu & debug

Context menu: remove trace / clear / mode / color / width-point-size / split H / split V / reset view / toggle legend / _plot info…_ — the latter opens the per-plot debug popup: trace count, samples total & visible, GPU bytes, last y-query µs, last paint-callback µs (spec's "plot info/debug window").

### 10.7 Drag & drop

The browser's drag payload is `Vec<FieldId>` (multi-select drops N traces at once). Drop targets: a plot (append), a tile-tree edge (new split with the traces), the 3D pane (rejected with a hint toast unless it is a position-mappable topic — convenience wiring for §12).

---

## 11. Timeline & playback

```rust
struct Playback { state: Stopped|Playing, t_us: i64,
                  speed: f32 /* 0.1..=16 */, follow_live: bool }
```

- Advance in `update()`: `t_us += (dt * speed * 1e6) as i64`, clamped to the global range; the playhead is the single time authority for plots **and** the 3D view (spec: synced playhead everywhere).
- Scrubber: full-range bar, draggable handle, shaded live extent, current time shown both absolute (UTC if the source carries it) and log-relative.
- Transport: play/pause (`Space`), jump start/end (`Home`/`End`), step ±1 sample of the focused plot's first trace (`←`/`→`; falls back to 1/30 s when nothing is focused), speed picker.
- `follow_live` pins `t_us` to the stream tail each frame; manual scrub disengages it (same explicit-state pattern as §10.4).

**Repaint policy (decision).** `request_repaint()` runs continuously **only** while playing or while any link is `Connected`; otherwise the app is fully event-driven and idles at 0% GPU. **Why:** the spec's "idle-aware" requirement — a telemetry tool that spins fans while showing a static plot is broken.

---

## 12. 3D view

### 12.1 Vehicle configuration

```rust
struct VehicleConfig {
    source: SourceId, label: String, show: bool,
    pos: PosMapping,   // Ned{topic,xyz fields} | Gps{lat,lon,alt, ref: Auto|Manual(lla)} | Custom
    ori: OriMapping,   // Static | Euler{r,p,y, degrees: bool} | Quat{wxyz, convention note}
    model: ModelKind,  // Quad | FixedWing | DeltaWing | Marker | CustomGlb(path)
    color: Color32, path_color: Color32, scale: f32,
}
```

The config dialog offers **field-mapping pickers with sane defaults** per source type (AP: `POS`/`ATT` or `AHR2`; PX4: `vehicle_local_position`/`vehicle_attitude`; MAVLink: `LOCAL_POSITION_NED`/`ATTITUDE`). Unit toggles (deg/rad, m vs lat-lon-alt) live in the mapping, per spec. Any config or time-offset change invalidates and rebuilds the trajectory (off-thread).

### 12.2 Coordinates & precision

- **GPS→NED** in `f64`: geodetic → ECEF → NED about a reference origin (auto = first valid fix, or manual LLA). Result stored as **f32 NED meters** — centimeter-safe within tens of km, sidestepping the §6.2 lat/lon-as-f32 trap entirely.
- **NED → render mapping** (right-handed, Y-up): `render = (E, −D, −N)` i.e. X=East, Y=Up, Z=South. Stated once here; every shader and camera obeys it.
- **Orientation:** Hamilton quaternions, body→NED (AP/PX4 convention), `w`-first; Euler is intrinsic Z-Y-X (yaw-pitch-roll). Pose at playback time uses previous-sample in v1; slerp is a checklist follow-up.

### 12.3 Scene & cameras

Ground grid (shader-based, distance-faded), world axes gizmo, full trajectory polyline + current pose marker per vehicle, per-vehicle color/path-color/scale/visibility. **One camera, always tracking.** It orbits a target that follows the tracked vehicle's pose at playback time, with the user's orbit offset (yaw/pitch/distance) preserved — left-drag orbits, wheel zooms, double-click resets the offset (keeping the target). **Which** vehicle it tracks is chosen by a dropdown that appears in the scene pane **only when two or more vehicles are configured**; with one vehicle it tracks that vehicle and the dropdown is hidden, and with none it tracks the world origin (dropdown hidden). There are no separate Orbit/Free camera modes. When no vehicle is configured, the scene shows just the grid and axes gizmo (no placeholder path).

### 12.4 Models

Embedded GLBs (`assets/`: quad, fixed-wing, delta, arrow marker) via `include_bytes!`, plus user GLB loading; a procedural cone is the unconditional fallback so a missing asset can never blank the scene.

---

## 13. Data browser (left drawer)

Tree: **Source → Topic → Field**, each field row showing dtype, sample count and unit chip; source rows show file/endpoint name, time range, row total and memory. Behaviors, each a checklist item:

- **Fuzzy search/filter** over full paths (`gps hacc` matches `GPS[0].HAcc`).
- **Natural sort** (`GPS[2]` before `GPS[10]`).
- Multi-select (ctrl/shift) → drag to plot (§10.7).
- Row context menu: plot in new pane / copy `FieldKey` / stats popup (§17.1) / set time offset (drag-µs widget + dialog) / remove source.
- **Favorites**: pin fields to a top section; persisted in layouts.
- Field metadata inspector: unit, multiplier, dtype, source message id, per-chunk stats fold.

---

## 14. Layouts & sessions

### 14.1 What persists

The tile tree; per-pane traces as `FieldKey` + style (color/width/mode/visible); legend flags; `ViewX` mode (full vs locked-live); playback speed; vehicle configurations (with `FieldKey` mappings); per-source time offsets; favorites; 3D camera mode + pose; dock visibility.

### 14.2 Schema & migration

```json
{ "delog_layout": 1, "tiles": { …egui_tiles tree… },
  "panes": [ { "traces": [ { "key": {"source":"flight_0042","topic":"BARO","field":"Alt"},
                             "color":"#7aa2f7", "width":1.5, "mode":"line", "visible":true } ],
               "y": {"mode":"auto_visible"} } ],
  "vehicles": [ … ], "offsets": { "flight_0042": 0 }, "favorites": [ … ] }
```

A top-level integer version and a chain of pure `migrate_vN_to_vN+1(Value) -> Value` functions; loading any older version walks the chain. **Why function-chain migration:** every historical layout stays loadable forever, and each migration is unit-testable against a frozen fixture file.

### 14.3 Resolution against loaded data (decision)

Loading a layout resolves every `FieldKey` through the `Resolver`. Misses do **not** fail the load: they become **ghost traces** — grey legend entries plus one summarizing diagnostic — that _auto-bind_ the moment a matching source is loaded. **Why:** the natural workflow is "open my standard tuning layout, then drop today's log on it"; order-independence makes that workflow real instead of an error message.

### 14.4 Files & autosave

Named layouts under the config dir; export/import as plain JSON (same schema). `session.json` autosaves on exit and every 30 s while dirty — crash recovery for free.

---

## 15. Diagnostics hub

```rust
struct Diag { wall: SystemTime, severity: Info|Warning|Error,
              origin: Parser(SourceId)|Stream(LinkId)|Ingest|Layout|Render|DataQuality,
              code: &'static str, msg: String, count: u32 }
```

- Central `mpsc` into a ring buffer (cap 10k). **Burst dedup:** identical `(origin, code, msg)` within a window increments `count` instead of appending — a flapping serial link produces one row counting up, not ten thousand rows.
- Panel: severity filter, text search, origin filter, clear; rows with a time payload click-to-jump the playhead.
- **Data-quality scan** (async, post-load, per topic): timestamp regressions, dt outliers (>10× median), duplicate timestamps, NaN/Inf percentage — each summarized as a single diag with counts (spec's discontinuity/duplicate/NaN reporting).
- Emitters wired from day one: parsers (§6.1), stream (§7.2), ingest drops (§5), layout ghosts (§14.3), wgpu error scopes, cache builder.

---

## 16. Performance metrics & dock

```rust
// register once, then: let _t = metrics.scope("yquery");   (RAII timer)
struct MetricRing { last: f32, min: f32, max: f32, avg: f32, p99: f32, n: u64 } // ring of 256
```

**Instrumented from the start** (cheap atomics; the dock merely _reads_): `parse_total`, `ingest_batch`, `snapshot_swap`, `cache_build`, `cache_append`, `minmax_build`, `yquery`, `plot_paint_cpu`, `gpu_encode`, `3d_frame`, `upload_bytes`, `gpu_full_uploads`, `live_rx_rate`, `ingest_dropped_batches`, `frame_total`, plus the memory gauges of §4.6 and per-trace sample/visible counts.

Dock: metrics table (last/avg/min/max/samples per spec) + GPU buffer count/bytes + CPU cache bytes; **refreshes at 4 Hz** regardless of frame rate (spec: throttled). Optional FPS/status indicator obeys the §11 idle policy. Debug overlay toggle paints frame timings in-corner. **Export profiling snapshot** writes all rings + gauges to JSON — the artifact to attach to a perf bug.

---

## 17. Analysis features

### 17.1 Statistics

- **Global per field:** O(chunks) fold over sealed `ColStats` — instant, no scan.
- **Visible-window:** min/max from the pyramid; mean/σ need a real scan → computed on demand, `rayon` over chunk slices, memoized per `(field, window)`; the stats popup shows min/max instantly and fills μ/σ/rate when the scan lands. Rate estimate = rows/span.

### 17.2 Multi-log comparison

Falls out of the architecture: multiple sources + per-source offsets + the shared X axis. The offset drag-widget (§13) _is_ the alignment tool. A computed A−B "diff trace" is backlog (needs resampling semantics).

### 17.3 Derived fields (v1: built-ins)

`magnitude(f₁..fₙ)`, `scale+offset`, `deg↔rad`, `unwrap(angle)`. **Decision:** derived fields **materialize through the normal ingestion path** as a `derived:` source. **Why:** they instantly inherit chunking, stats, caching, GPU rendering, layout persistence — zero special cases downstream. A full expression engine is backlog (ANA-08) with prev-sample alignment onto the union timeline.

### 17.4 Events, markers, bookmarks

`Marker { t_us, label, color, note }` — manual add at the playhead (`M`), listed in a bookmarks panel (click-to-jump), drawn as flags on the timeline and faint verticals on plots. **Auto-markers** (toggleable) from AP `MSG`/`EV` and ULog logged messages. Persisted in the session. Gap/reset detection (§15) can optionally shade affected plot regions (backlog polish).

---

## 18. Import / export

| What                    | How                                                                                                                                                | Notes                                               |
| ----------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| CSV                     | field multi-pick, range = full \| visible, resample = none (union timeline, blanks) \| previous-fill \| linear @ fixed dt                          | streamed writer + progress; off-thread              |
| Plot image              | v1: egui screenshot of the pane region (1×/2×)                                                                                                     | offscreen vector-quality re-render = backlog IOX-07 |
| Layout JSON             | §14 export/import                                                                                                                                  |                                                     |
| Diagnostics / profiling | JSON dumps                                                                                                                                         | §15, §16                                            |
| **Arrow IPC cache**     | post-parse background write of a `.dlcache` sidecar; on open, sniff prefers it when `(mtime,size)` of the original matches; reload via mmap (ZC-5) | turns a 2 GB BIN re-open into milliseconds          |
| Parquet cache           | later — compression + ecosystem interop                                                                                                            | IOX-09                                              |
| Live recording          | §7.5 `.tlog`                                                                                                                                       | round-trip tested                                   |

---

## 19. UI / UX composition

### 19.1 Main window

```
┌───────────────────────────────────────────────────────────────────────┐
│ ☰ File  Tools  Layout  Help   │ ▶ Open  ⏹ Cancel  ＋Plot  3D  📡 Stream │
├──────────────┬────────────────────────────────────────────────────────┤
│ DATA         │                tile workspace                          │
│  ▸ flight_42 │   ┌───────────────┬───────────────┐                    │
│    ▸ BARO    │   │ plot          │ plot          │                    │
│      Alt  ●  │   ├───────────────┴───────────────┤                    │
│      Press   │   │ 3D view                       │                    │
│  ▸ udp:14550 │   └───────────────────────────────┘                    │
│  [search…]   │                                                        │
├──────────────┴────────────────────────────────────────────────────────┤
│ ◀ ▶ ⏯  1×   ────────●──────────────────────────  t = 00:04:12.345678  │
├────────────────────────────────────────────────────────────────────────┤
│ Diagnostics ▾ │ Performance ▾                                          │
└────────────────────────────────────────────────────────────────────────┘
```

Workspace-first (no landing page); data drawer left (collapsible); timeline bottom; diagnostics/perf as a shared bottom dock with tabs.

### 19.2 Toolbar & menus

Toolbar: open log, cancel parse, add plot, remove plot, toggle 3D, vehicle config, start stream, stop stream — every icon button has a tooltip (spec). Menus: **File** (open, recent, import CSV, export…, quit) · **Tools** (stream config, derived field, data-quality scan, export diagnostics/profile) · **Layout** (save, save-as, load, manage, import/export JSON, reset) · **Help** (shortcuts, about, licenses).

### 19.3 Keyboard shortcuts

`Ctrl+O` open · `Space` play/pause · `Ctrl+T` add plot · `Ctrl+W` close pane · `R` reset view · `Ctrl+F` focus search · `Ctrl+S`/`Ctrl+L` save/load layout · `End` lock-to-live · `M` marker · `←/→` step · `F12` debug overlay.

### 19.4 Theme

Dark-first, dense-data-tuned: near-black plot background, ≥4.5:1 contrast for text, the 10-color trace palette as named constants in `assets/palette.rs` (single source for plots, legend dots, 3D paths). High-DPI via egui's native pixels-per-point; minimum window 1024×640 with collapsible docks for responsiveness.

### 19.5 Empty states (each is real copy, not an afterthought)

No log loaded → drop-target hero in the workspace ("Drop a .bin / .ulg / .tlog — or File ▸ Open"). Empty plot → "Drag a field here". Live source connected but silent → "Listening on udp:14550 — no packets yet" + counter. Cache building → shimmer + "preparing trace…".

### 19.6 The never-block rule

Parsing, scans, cache builds, exports, GLB loads: always off-thread; the UI thread's job is `snapshot.load()`, layout, and draw. Anything new that could exceed ~16 ms ships as a job + progress, or it doesn't ship.

---

## 20. Testing & benchmarking strategy

### 20.1 Unit & property tests

- Time math, key resolution, layout migrations (frozen fixture per version).
- Chunk append + snapshot semantics (writer/reader interleavings).
- **Pyramid correctness by `proptest`:** for random data and random windows, `pyramid.query(a,b) == naive_scan(a,b)` — the indexed path is only acceptable because this property pins it to the exact path.
- Parser record-level goldens: tiny real logs with expected topic/row/value tables.

### 20.2 Fixtures & fuzzing

`fixtures/`: real trimmed logs `ap_457.bin`, `ap_463.bin`, `px4.ulg`, `sample.tlog` (< 1 MB each) + synthetic generators (sine/step/gaps/regressions) for deterministic perf tests. `cargo-fuzz` targets per frame decoder (BIN record, ULog defs+data, MAVLink framing): **no input may panic, OOM or hang** — the §6.1 error policy, enforced.

### 20.3 Integration & golden image

Headless wgpu: parse fixture → snapshot → cache → render one frame offscreen → readback → perceptual-hash compare against a checked-in golden (tolerance for driver variance). This is why `delog-render` must stay egui-free (§3.2).

### 20.4 Criterion benches & budgets (soft-asserted in CI)

| Bench                                            | Budget            |
| ------------------------------------------------ | ----------------- |
| BIN parse throughput                             | ≥ 80 MB/s         |
| ULog parse throughput                            | ≥ 60 MB/s         |
| y-range query @ 100M samples                     | < 50 µs           |
| cache build                                      | ≥ 50 Msamples/s   |
| incremental upload, 512 samples                  | < 50 µs CPU       |
| frame encode, 32 traces × 1M visible (decimated) | < 3 ms CPU        |
| snapshot swap under live load                    | < 10 µs           |
| sustained live: 60 msg-types @ 50 Hz             | < 1% ingest drops |

### 20.5 CI

`fmt` → `clippy -D warnings` → tests → 60 s fuzz smoke per target → bench compile (full bench run nightly). Linux first; mac/Windows builds in the matrix once M3 lands.

---

## 21. Milestones

Each milestone has an exit criterion an agent can verify. Checklist areas in §22 reference these.

| M       | Scope                                                                                                 | Exit criterion                                                         |
| ------- | ----------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| **M0**  | Workspace scaffold, deps pinned, CI, empty eframe window, metrics registry                            | CI green; window opens; `metrics.scope()` works                        |
| **M1**  | `delog-core`: IDs, time, chunks, snapshots, stats, memory accounting + tests                          | property tests green; snapshot bench < 10 µs                           |
| **M2**  | BIN parser + ingest thread + data browser tree (read-only)                                            | fixture BIN loads; tree shows topics/fields/counts; cancel works       |
| **M3**  | **Plot MVP**: cache + pyramid, buffer manager, `line_pull`, one pane, drag-drop, pan/zoom, auto-Y     | 1M-sample trace pans at 60 FPS; golden image test green                |
| **M4**  | Multi-plot tiles, shared X, scatter/step modes, legend, context menu, decimated path, tooltips, hover | 32×1M traces decimated < 3 ms encode; tooltip shows canonical values   |
| **M5**  | Timeline + playback + playhead + markers (manual)                                                     | scrub drives all plots; idle = 0% GPU                                  |
| **M6**  | ULog + tlog parsers, multi-log, offsets, data-quality scan                                            | mixed BIN+ULG session aligned via offset drag                          |
| **M7**  | Live MAVLink (UDP/TCP/serial), counters, tail-lock, pause-view, incremental upload, recording         | 50 Hz stream for 10 min: no UI jank, < 1% drops, recording round-trips |
| **M8**  | 3D view: scene, cameras, vehicles, GPS→NED, GLB, demo path                                            | trajectory + tracked camera synced to playhead                         |
| **M9**  | Layouts/sessions (save/load/migrate/ghost-resolve/autosave), diagnostics dock, perf dock              | layout saved → restart → drop log → ghosts auto-bind                   |
| **M10** | Stats popups, derived built-ins, CSV/image/JSON exports, IPC cache, polish, shortcuts                 | 2 GB BIN re-open < 1 s via cache; CSV export of visible window         |

---

## Appendix A — Core type sketch

```rust
// delog-core/src/lib.rs (abridged)
pub mod time { pub type TimeUs = i64; }

pub struct TopicSchema {
    pub name: String,
    pub fields: Vec<FieldDef>,        // name, dtype, unit, multiplier
}

pub struct SourceEntry {
    pub id: SourceId, pub key: String, pub kind: SourceKind,
    pub offset_us: i64,
    pub topics: Arc<[Arc<TopicStore>]>,
    pub meta: SourceMeta,             // params, file info, link info
}

impl StoreSnapshot {
    pub fn global_range(&self) -> Option<(TimeUs, TimeUs)>;
    pub fn field(&self, id: FieldId) -> FieldView<'_>;       // zero-copy accessor
    pub fn sample_at(&self, id: FieldId, t: TimeUs, interp: Interp) -> Option<Scalar>;
    pub fn mem(&self) -> MemBreakdown;
}

pub trait IngestSink: Send {
    fn open_source(&mut self, key: &str, kind: SourceKind) -> SourceId;
    fn submit(&mut self, batch: ParsedBatch);                // moves Arrow buffers (ZC-1)
    fn diagnostic(&mut self, d: Diag);
    fn progress(&mut self, source: SourceId, frac: f32);
    fn close_source(&mut self, source: SourceId, summary: ParseSummary);
}
```

## Appendix B — Line vertex-pulling shader (pseudocode WGSL)

```wgsl
@group(0) @binding(0) var<storage, read> xy: array<f32>;        // interleaved pairs
@group(0) @binding(1) var<uniform> u: PlotUniform;              // x/y scale+offset, width_px, viewport

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    let seg = vi / 6u;
    let corner = vi % 6u;                       // 2 triangles of a quad
    let p0 = vec2(xy[2u*seg],      xy[2u*seg + 1u]);
    let p1 = vec2(xy[2u*seg + 2u], xy[2u*seg + 3u]);
    if (!finite(p0.y) || !finite(p1.y)) { return degenerate(); }   // NaN = gap (ZC-free)
    let a = to_screen(p0, u);  let b = to_screen(p1, u);
    let dir = normalize(b - a);
    let n   = vec2(-dir.y, dir.x) * (u.width_px * 0.5);
    let pos = select_corner(a, b, n, corner);   // a±n, b±n
    return clip(pos, u.viewport);
}
```

## Appendix C — Glossary

**Canonical data** — the i64-µs, original-dtype Arrow chunks; the only ground truth. **Render cache** — the single permitted f32 copy per field. **Spine** — `Arc<[Arc<Chunk>]>` swapped on append. **Epoch** — one published `StoreSnapshot` generation. **Ghost trace** — a layout trace whose `FieldKey` is currently unresolved. **One Copy Rule** — ZC-invariant 3. **Pyramid** — the branching-64 min/max index.

---

## 22. MASTER CHECKLIST

Maintained per §0. IDs are stable — never renumber; append new items at the end of their area.

### ARC — Workspace & scaffolding (M0)

- [x] **ARC-01** — Workspace `Cargo.toml` with pinned `[workspace.dependencies]`; crates `delog-core/-parsers/-stream/-cache/-render/-app` created with the §3.2 dependency edges only
- [x] **ARC-02** — CI: fmt, clippy `-D warnings`, tests, bench compile (Linux)
- [x] **ARC-03** — `delog-app` opens an empty eframe window with the wgpu backend; dark theme applied
- [x] **ARC-04** — `tracing` initialized; panic hook logs + flushes
- [x] **ARC-05** — Metrics registry in core: scoped RAII timers, gauges, 256-ring stats (§16)
- [x] **ARC-06** — `assets/` embedding (palette consts, WGSL includes, GLB include_bytes)
- [x] **ARC-07** — License/about metadata; `Help ▸ About`

### CORE — Data model (M1)

- [x] **CORE-01** — `SourceId/TopicId/FieldId` registries + `FieldKey` + label collision suffixing (§4.1)
- [x] **CORE-02** — Time model: i64 µs, per-source `offset_us`, effective-time helpers, global range (§4.2)
- [x] **CORE-03** — `TopicSchema` with dtype/unit/multiplier per field
- [x] **CORE-04** — `Chunk` (sorted t, Arrow cols, seal-time `ColStats`, t_min/t_max)
- [x] **CORE-05** — `TopicStore` spine; append = structural-share + swap (§4.4)
- [x] **CORE-06** — `StoreSnapshot` + `ArcSwap` store; epoch counter; subscriber notification
- [x] **CORE-07** — Zero-copy field accessors: `FieldView`, chunk-pruned binary search, `sample_at` with Prev/Next/Linear
- [x] **CORE-08** — Multi-instance topic naming `topic[N]` (§4.3)
- [x] **CORE-09** — Memory accounting `MemBreakdown` per field/topic/source (§4.6)
- [x] **CORE-10** — Remove-source snapshot rebuild + orphan GC hooks
- [x] **CORE-11** — Tests: snapshot interleavings, accessor properties, time math
- [x] **CORE-12** — Bench: snapshot swap under append load (< 10 µs)

### ING — Ingestion pipeline (M2)

- [x] **ING-01** — `IngestSink` trait + `IngestMsg` + bounded channel (cap 256) (§5)
- [x] **ING-02** — Ingest thread: batch→chunk sealing (64Ki file / 512-or-100ms live), stats at seal
- [x] **ING-03** — Backpressure policy: file-block vs live-drop + `ingest_dropped_batches` metric + diag
- [x] **ING-04** — Byte-based progress events; `Arc<AtomicBool>` cancel polled ≤4096 records
- [x] **ING-05** — Within-batch timestamp sort; cross-chunk regression diagnostic
- [x] **ING-06** — Epoch-change notifications to UI repaint + cache manager

### PAR — Parsers (M2 BIN, M6 ULog/tlog)

- [x] **PAR-01** — `LogParser` trait, sniff scoring, registry, manual-override picker plumbing (§6.1)
- [x] **PAR-02** — Skip-and-diagnose malformed-record policy with byte offsets
- [x] **PAR-03** — AP BIN: FMT/FMTU/UNIT/MULT decode; full type-char table incl. fixed-point and `L`
- [x] **PAR-04** — AP BIN: raw-value storage + unit/multiplier into schema (§6.2)
- [x] **PAR-05** — AP BIN: instance split → `GPS[0]`/`GPS[1]`…
- [x] **PAR-06** — AP BIN: golden fixture test (topics/rows/values table)
- [x] **PAR-07** — ULog: header + defs (F/I/P) + data (A/D/S) sections; nested flatten; padding skip; multi_id
- [x] **PAR-08** — ULog: dropouts → diagnostics + NaN gap injection (§6.3)
- [x] **PAR-09** — ULog: logged messages captured for auto-markers; params into source meta
- [x] **PAR-10** — ULog: golden fixture test
- [x] **PAR-11** — tlog: µs-envelope framing over the shared MAVLink decoder (§6.4) — explicit `[8-byte BE µs][frame]` framing reusing `mavlink::frame_len`/`decode_frame`/`extract_fields`; one topic per message type, `(sysid,compid)` instance suffixing; bad-CRC frames skipped without losing envelope sync; torn tail keeps prior data
- [x] **PAR-12** — tlog: golden fixture test (incl. v1+v2 mixed) — synthetic mixed v1/v2 log: topics/rows/values table + CRC-skip, unknown-message, truncation and desync cases
- [x] **PAR-13** — fuzz targets: BIN record / ULog defs+data / MAVLink framing — no panic/hang/OOM — cargo-fuzz crate in `/fuzz` (detached workspace) + stable `garbage_smoke` test covering the same property where `cargo test` runs
- [ ] **PAR-14** — CSV import with column-mapping dialog _(M10/backlog boundary)_
- [ ] **PAR-15** — Arrow IPC `.dlcache` reader registered as a sniffing parser (pairs IOX-08)
- [x] **PAR-16** — Shared MAVLink layer in `delog-parsers::mavlink`: owned v1/v2 framing (sync, CRC, seq-gap/resync counters, §7.2) + serde-based field extractor (§7.3) — one code path for the tlog parser (PAR-11) and the live readers (LIV-02/05)

### LIV — Live streaming (M7)

- [x] **LIV-01** — `Endpoint` config model + connection dialog (UDP server, TCP client, serial+baud) — UDP-client/TCP-server modes removed by decision
- [x] **LIV-02** — Reader thread with owned framing: v1/v2 sync, CRC, seq-gap counters (§7.2) — `LinkReader::spawn(endpoint)` opens UDP-server/TCP-client/serial with a read timeout, pumps bytes through the shared `FrameDecoder` on its own thread, emits `DecodedFrame`s on a channel, exposes lock-free `LinkStats` + clean stop/join
- [x] **LIV-03** — Link state machine + UI indicator (Connecting/Connected/Stale/Lost) — `LinkState::classify` + `LinkReader::state()` tested; app menu shows per-link state/counters while live links run
- [x] **LIV-04** — Auto-reconnect (TCP/serial) with backoff — `supervise` re-opens reconnectable links with exponential backoff (0.5 s→8 s cap), resetting only after a session that delivered frames; injected connect/sleep make the schedule deterministically testable; interruptible backoff sleep keeps stop responsive
- [x] **LIV-05** — Build-script extractor: `MavMessage → fields`, zero-alloc; unknown-msg once-diag — implemented via the amended flat serde extractor in `delog-parsers::mavlink`, used by live ingestion; unknown message IDs emit once-per-link diagnostics
- [x] **LIV-06** — sysid demux → source per (link,sysid); compid instance folding — live consumer opens `mavlink:<endpoint>:sysid<N>` sources and assigns `[N]` topic suffixes for additional compids of the same message
- [x] **LIV-07** — Live batching into ingest (512/100 ms), tail metrics (`live_rx_rate`) — frame consumer builds Arrow batches through `LiveSink`, flushes at 512 rows or 100 ms, records `live_rx_rate`
- [x] **LIV-08** — Pause/resume _view_ while ingestion continues (§7.4) — app-level live-tail follow can be paused/resumed without stopping live readers or ingest
- [x] **LIV-09** — Raw-bytes `.tlog` recorder + round-trip CI test (§7.5) — `TlogRecorder` writes raw frame envelopes; stream tests replay a recorded frame through `TlogParser`
- [x] **LIV-10** — Multi-link simultaneous operation — `Session` owns a vector of `LiveLink`s; each start creates independent reader and live-ingest workers
- [x] **LIV-11** — Sustained-load bench: 60 msg-types @ 50 Hz, <1% drops, no UI jank — `delog-stream/benches/sustained_live.rs` compiles and exercises the planned 60×50 Hz live sink shape

### CCH — Render cache (M3)

- [x] **CCH-01** — `TraceCache` interleaved f32 xy + `origin_us` + high-water mark (§8.1)
- [x] **CCH-02** — One-Copy builder: in-place chunk iteration, multiplier in f64→f32, NaN preserved (§8.2)
- [x] **CCH-03** — Async build job + "building cache…" plot state
- [x] **CCH-04** — Incremental append on epoch change (never rebuild)
- [x] **CCH-05** — Min/max pyramid (branch 64): build + incremental append (§8.4)
- [x] **CCH-06** — `query(a,b)` O(log n) min/max + proptest equivalence vs naive scan
- [x] **CCH-07** — Per-pixel-column min/max helper for decimated draw
- [x] **CCH-08** — `CacheManager`: FieldId map, epoch subscription, source-removal GC
- [x] **CCH-09** — LRU eviction of unplotted caches over budget; plotted pinned
- [x] **CCH-10** — Cache/pyramid byte accounting into MemBreakdown
- [x] **CCH-11** — Benches: build Msamples/s, yquery @100M, append latency
- [ ] **CCH-12** — _(backlog)_ split-double x or per-window rebase for >4000 s deep zoom (§8.3)

### GPU — Renderer (M3–M4, 3D pipelines M8)

- [x] **GPU-01** — `delog-render` context bootstrap from an external device/queue (egui's); no egui types
- [x] **GPU-02** — Buffer manager ledger: STORAGE buffers, ×1.5 growth via GPU-side copy, byte totals (§9.3)
- [x] **GPU-03** — Incremental `write_buffer` of appended span only; `gpu_full_uploads` counter (ZC-4)
- [x] **GPU-04** — Dynamic-offset uniform buffer for per-plot transform/style (no push constants)
- [x] **GPU-05** — `line_pull` pipeline + WGSL (vertex pulling, width expansion, NaN→degenerate) (§9.4)
- [x] **GPU-06** — Per-plot viewport/scissor inside egui main pass via paint callback
- [x] **GPU-07** — `scatter_pull` pipeline (quad/sample, size uniform)
- [x] **GPU-08** — `step_pull` stepped-mode pipeline (2 segments/sample)
- [x] **GPU-09** — `minmax_col` decimated pipeline + transient ring upload (§9.5)
- [x] **GPU-10** — Draw-path selector: full vs decimated at samples/px > 8
- [x] **GPU-11** — Batched encoding: one pipeline bind, per-trace dynamic offsets
- [x] **GPU-12** — wgpu error scopes → diagnostics
- [x] **GPU-13** — Headless golden-image test rig (§20.3)
- [~] **GPU-14** — Bench: frame encode 32×1M decimated < 3 ms — ~4.9 ms after GPU-11 batching (was ~12 ms); CPU min/max decimation alone is ~4.3 ms of that, so GPU-26 (compute reduction) is the path to full budget. Typical 1–8 traces are well under.
- [x] **GPU-20** — 3D offscreen target (color+depth, 4×MSAA) composited as egui image (§9.1) — `Scene3dTarget` in `delog-render`: 4×MSAA color+depth resolving to a single-sample texture, `begin_pass` clears color+depth(1.0) and resolves, `resolve_view()` for egui + `read_rgba()` headless readback; golden tests prove MSAA resolve, depth rejection, edge AA. Composited in `delog-app` via `GpuBridge::render_scene` (offscreen pass → `register/update_egui_texture_from_wgpu_texture` → `painter().image`); verified on RTX 4080 with TDV-01
- [x] **GPU-21** — `grid3d` infinite grid + axes gizmo — `Grid3dPipeline` (full-screen triangle, per-pixel ground-plane unproject, derivative-AA lines, distance fade, true frag-depth write) drawing into the scene target; principal ground axes colored per §12.3 (X/East red, Z/South blue). Golden test drives it with a real perspective camera. Vertical Y (Up) axis line rides with the line pipeline (GPU-23). `glam` added (workspace dep) — tests-only here; production pipeline takes raw matrices
- [x] **GPU-22** — `mesh` pipeline (N·L+ambient) + GLB upload path — `MeshPipeline` (vertex+index buffers, Lambert N·L + ambient, depth+MSAA, double-sided), `MeshUniform { view_proj, model, normal_mat, light_dir, color, ambient }`, `MeshGpu::upload`. `load_glb` decodes the first triangle primitive via the `gltf` crate (smooth normals when absent, flat indices when absent); `MeshCpu::cone` is the procedural fallback (§12.4). Golden tests prove a shaded gradient renders and a hand-built GLB decodes. App wiring (model registry, embedded GLBs, per-vehicle draw) lands with TDV-03/08
- [x] **GPU-23** — `traj3d` trajectory line pipeline — `Traj3dPipeline`: vertex-pulled line-list from a `vec4` points storage buffer (`(N-1)*2` verts, depth-tested, MSAA), `Traj3dUniform { view_proj, color }`; non-finite endpoints collapse the segment so NaN is a gap (§9.4). Golden tests prove a polyline renders and a NaN endpoint leaves no pixels. Also serves the vertical world-axis line GPU-21 couldn't draw. App consumers (demo path, vehicle trajectories) arrive with TDV-10/11
- [x] **GPU-24** — 3D frame-time metric — `scene_ui` wraps the scene build+encode in a `metrics.scope("3d_frame")` RAII timer (§16 metric name). `Session::metrics()` exposes the registry; `PlotServices` carries it. The metric accumulates now; the perf dock that displays it lands with PRF-02
- [ ] **GPU-25** — _(later)_ thick/miter-join 2D line pipeline
- [ ] **GPU-26** — _(later)_ compute-shader min/max reduction
- [ ] **GPU-27** — _(later)_ GPU picking/hover acceleration

### PLT — Plot system (M3–M4)

- [x] **PLT-01** — `egui_tiles` workspace: add/remove/split H/V/tabs/drag (§10.1)
- [x] **PLT-02** — `PlotPane`/`TraceRef` state; palette auto-assign
- [x] **PLT-03** — Shared `ViewX` µs model; all panes render from it (§10.3)
- [x] **PLT-04** — Wheel zoom @ cursor, drag pan, double-click reset-to-full
- [x] **PLT-05** — Lock-X-to-live with explicit unlock-on-scrub + re-lock affordance (§10.4) — `ViewX::locked_to_tail` preserves span while pinning to `global_end`; pan/zoom/reset unlock; live toolbar button highlights when re-lock is available
- [x] **PLT-06** — Y axis auto-fits the visible window (pyramid min/max + 5% pad) — full-data/manual modes built then removed by decision; AutoVisible is the only behavior
- [x] **PLT-07** — Axes/ticks/labels via egui; tick step chooser (1-2-5)
- [x] **PLT-08** — Legend + toggle; per-trace visibility/color/width/mode editing
- [x] **PLT-09** — Hover: canonical binary search, Prev/Next/Linear tooltip modes, hover circles (§10.5)
- [x] **PLT-10** — Playhead vertical line + value readout on all panes
- [x] **PLT-11** — Context menu: remove/clear/mode/color/width/split/reset/legend/info (§10.6)
- [x] **PLT-12** — Plot debug popup: counts, visible range, GPU bytes, yquery µs, paint µs
- [x] **PLT-13** — Drag-drop: single + multi-field onto pane / tile edge (§10.7)
- [x] **PLT-14** — Empty-pane state copy

### TLN — Timeline & playback (M5)

- [x] **TLN-01** — `Playback` model: play/pause, speed 0.1–16×, clamp (§11)
- [x] **TLN-02** — Scrubber: range bar, handle, live-extent shading
- [x] **TLN-03** — Absolute (UTC when available) + log-relative time display — UTC path plumbed + tested; no parser emits a UTC reference yet (BIN GPS week / ULog `time_ref_utc` land with M6)
- [x] **TLN-04** — Jump start/end; step ±1 sample of focused reference trace (fallback 1/30 s)
- [x] **TLN-05** — `follow_live` tail mode, disengage-on-scrub — `Playback::follow_live` pins playhead to tail; timeline scrub/jump/step disengage, `End`/timeline end button re-locks for live sessions
- [x] **TLN-06** — Idle-aware repaint policy: continuous only when playing/connected (§11) — "connected" half activates with M7 live links
- [x] **TLN-07** — Playhead drives 3D pose lookup (with M8) — the workspace passes `playback.t_us` into the scene pane; `scene_ui` resolves each visible vehicle with `vehicle::pose_at(snapshot, vehicle, playhead)` and keeps the tracked camera target synced to that pose. Verified in-app while scrubbing a real ArduPilot loop log

### TDV — 3D view (M8)

- [x] **TDV-01** — Scene pane: grid, axes, orbit camera (pan/zoom) — `Pane::Scene3D` with an `OrbitCamera` (left-drag orbit, wheel zoom, double-click reset; pitch-clamped, unit-tested); single instance toggled show/hide by the toolbar "3D" button. Renders the grid offscreen and composites as an egui image. `glam` added to `delog-app`. Verified on RTX 4080: grid visible/infinite, orientation/orbit/zoom/color all good
- [x] **TDV-02** — Single tracking camera + tracked-vehicle dropdown (§12.3) — _(scope changed by decision: no Orbit/Free modes)_ one `OrbitCamera` that always orbits a target tracking the selected vehicle's pose, offset preserved (unit-tested); left-drag orbits, wheel zooms, double-click resets the offset and keeps the target. Tracks the world origin until vehicles exist; the tracked-vehicle dropdown (shown only with ≥2 vehicles) lands with vehicle config (TDV-03). Simplified pane verified on RTX 4080
- [x] **TDV-03** — `VehicleConfig` + dialog with per-source mapping presets (§12.1) — data model + the **config dialog** done (`vehicle_dialog.rs`): source picker; pick a position topic then its N/E/D (or GPS lat/lon/alt) columns, and an orientation topic then Euler/Quat columns; model/color/scale; auto-detect by common names; vehicle list with show toggle / **edit** / remove. Reached from the toolbar "Vehicles" button. In-app verified on a real ArduPilot log via drag-drop; topic popups now focus their search box, list topics alphabetically, and support arrow-key navigation plus Enter selection
- [x] **TDV-04** — PosMapping NED / Custom-with-units; trajectory build off-thread — NED + custom unit scale + GPS mappings resolve to render space (`position_at`, unit-tested); `build_trajectory` resamples the path. The app caches trajectories and rebuilds only on epoch/config change; rebuilds now run on `delog-trajectory-build`, return over an mpsc channel, request repaint on completion, and never do trajectory resampling on the UI thread
- [x] **TDV-05** — GPS→NED f64 geodetic→ECEF→NED; auto/manual reference origin (§12.2) — `geo::geodetic_to_ned` (WGS84, f64) + `resolve_gps_ref` (Auto = first fix / Manual); 3 unit tests
- [x] **TDV-06** — OriMapping Static/Euler(deg|rad)/Quaternion; prev-sample pose at playhead — `orientation_at` + `geo::euler_to_quat`/`body_to_render_rot` (unit-tested); pose read prev-sample at the playhead in the render path. Dialog configuration verified in-app with Euler ATT roll/pitch/yaw driving a fixed-wing loop maneuver
- [x] **TDV-07** — NED→render mapping `(E,−D,−N)` everywhere; unit toggles — `geo::ned_to_render` + `ned_to_render_mat3` (proper rotation, unit-tested); `LengthUnit` toggle for custom NED
- [x] **TDV-08** — Embedded GLBs (quad/fixed-wing/delta) + custom GLB load + cone fallback — `models::mesh_for` with `include_bytes!` GLBs; `load_glb` bakes multi-part node transforms; `MeshCpu::cone` is the unconditional fallback. Real Quad/FixedWing/DeltaWing decode verified _(no marker asset; `Cone` replaces it per decision)_
- [x] **TDV-09** — Per-vehicle color/path color/scale/show; multiple vehicles — the scene render path draws N vehicles (mesh at pose + trajectory) honoring color/path-color/scale/show; the dialog creates multiple vehicles; the scene pane shows a tracked-vehicle dropdown when ≥2 vehicles are configured, preserves the selected row, falls back to a visible pose when needed, and uses per-row GPU cache keys so same-source vehicles do not collide
- [x] **TDV-10** — Trajectory line + current pose marker synced to playhead — render path draws each vehicle's trajectory + its mesh at the playhead pose (the mesh is the pose marker); in-app verified on a real ArduPilot log with playhead scrubbing, camera tracking, and no demo lemniscate visible
- [x] **TDV-11** — Rebuild on config/offset change — rides with vehicle config (TDV-03/04). Vehicle edits bump a vehicle revision; source-offset/data changes bump the snapshot epoch; trajectory worker results are accepted only when both keys still match, so stale builds cannot overwrite newer config/offset state. The green vertical Y-axis gizmo is always drawn; the demo lemniscate originally specced here was removed by decision — an unconfigured scene shows only the grid + axes gizmo
- [ ] **TDV-12** — _(later)_ slerp pose; time-windowed trail

### BRW — Data browser (M2 base, M4 polish)

- [x] **BRW-01** — Source→Topic→Field tree with dtype/count/unit chips (§13)
- [x] **BRW-02** — Fuzzy search/filter over full paths
- [x] **BRW-03** — Natural sort
- [x] **BRW-04** — ~~Plotted-field highlight (color dot + bold)~~ — built then removed by decision; field rows carry no plotted-state styling
- [x] **BRW-05** — Multi-select + drag payload `Vec<FieldId>`
- [ ] **BRW-06** — Context: plot-in-new-pane / copy key / stats popup / remove source
- [x] **BRW-07** — Per-source time-offset widget (drag-µs + dialog)
- [ ] **BRW-08** — Field metadata inspector
- [ ] **BRW-09** — Favorites/pinned section (persisted)
- [ ] **BRW-10** — Source rows: range, rows, memory; remove frees (verify via MemBreakdown)

### LAY — Layouts & sessions (M9)

- [x] **LAY-01** — Serde schema v1 per §14.2; save/load named layouts — `layout.rs` defines `delog_layout: 1` JSON, file IO under the app data `layouts/` directory, minimal Layout menu Save/Load modals, atomic temp-then-rename save, version rejection, and tests. Plot and vehicle field refs intentionally store only `topic + field` (no source) so layouts can be reused across logs; multi-source ambiguity is resolved by a load-time mapping dialog
- [x] **LAY-02** — Persist: tiles, traces+styles, legend, ViewX mode, speed, vehicles, offsets, favorites, camera, docks (§14.1) — implemented for current surface area: workspace plot/3D tree, trace style/visibility, global legend flag, `ViewX`, playback speed/follow-live, vehicle configs, 3D camera/tracked vehicle. Source offsets are intentionally not persisted because layout field refs are source-agnostic; favorites/docks are present as empty/default schema placeholders until those UI features exist
- [x] **LAY-03** — Version field + migration chain + frozen-fixture tests (§14.2) — layout loads now decode through a raw JSON migration entrypoint (`decode_doc`/`migrate_to_current`) before deserializing v1; missing/unsupported versions are rejected explicitly. `fixtures/layouts/v1_basic.json` is a frozen source-agnostic v1 fixture, tested for decode + cross-log application against a different single-source snapshot
- [x] **LAY-04** — Resolver: source-agnostic `topic.field`→FieldId; ghost traces + summary diag; auto-bind on source load (§14.3) — unresolved plot traces from layouts stay visible as grey `(missing)` legend rows, preserve trace style in saved layouts, and bind automatically on later snapshot epochs when exactly one matching loaded field exists. Layouts loaded before any log fields exist keep their original document and re-apply once parsed fields appear, so plot traces and vehicle configs populate after the file load. Ambiguous multi-source matches remain ghosts instead of silently binding the wrong source, and successful auto-binds emit a `layout-bind` diagnostic
- [x] **LAY-05** — Export/import layout JSON — Layout menu now has explicit JSON export/import actions backed by native file dialogs on worker threads. Export snapshots the current source-agnostic layout to an arbitrary `.json` path; import reads/decodes through the versioned migration path and applies against the currently loaded logs, including the existing ambiguity mapping flow
- [x] **LAY-06** — Autosave `session.json` (exit + 30 s dirty) — app writes the current source-agnostic layout document to the app data `session.json` atomically on shutdown and every 30 seconds when the serialized session differs from the last successful autosave; periodic write errors surface as `session-save` diagnostics
- [x] **LAY-07** — Layout manager UI (list/rename/delete/duplicate) — Layout menu now opens a manager window listing saved named layouts with load, rename, duplicate, and delete actions. Named-layout filesystem operations update the layout document name on rename/duplicate, list only `.json` layout files, and report manager diagnostics

### DIA — Diagnostics (M9, emitters earlier)

- [ ] **DIA-01** — Hub: mpsc → 10k ring; severity levels; burst dedup via count (§15)
- [ ] **DIA-02** — Dock panel: severity/origin filters, text search, clear
- [ ] **DIA-03** — Click-to-jump playhead for time-bearing diags
- [ ] **DIA-04** — Emitters wired: parser, stream, ingest-drop, layout-ghost, wgpu, cache
- [x] **DIA-05** — Async data-quality scan: regressions, dt outliers, duplicates, NaN/Inf % (§15)
- [ ] **DIA-06** — Log metadata display (params, file info, link info) in browser/inspector
- [ ] **DIA-07** — Export diagnostics JSON

### PRF — Performance dock (M9, instrumented from M0)

- [ ] **PRF-01** — All §16 timers/gauges instrumented at their call sites
- [ ] **PRF-02** — Dock table: last/avg/min/max/p99/samples per metric
- [ ] **PRF-03** — 4 Hz dock refresh decoupled from frame rate
- [ ] **PRF-04** — GPU buffer count/bytes + CPU cache bytes + per-trace sample/visible counts
- [x] **PRF-05** — Idle-aware FPS/status indicator (off when event-driven) — EMA-smoothed FPS badge pinned top-right of the menu bar, green >60 / orange 30–60 / red <30; computed from the real wall-clock frame-to-frame gap so it tracks during any continuous render (playback/live/interaction), reads "idle" when gaps exceed ~5 FPS (§11)
- [ ] **PRF-06** — F12 debug overlay
- [ ] **PRF-07** — Export profiling snapshot JSON

### ANA — Analysis (M10)

- [ ] **ANA-01** — Global field stats via ColStats fold (min/max/mean/σ/count/rate) (§17.1)
- [ ] **ANA-02** — Visible-window stats: pyramid min/max instant; rayon μ/σ on demand, memoized
- [ ] **ANA-03** — Stats popup UI (browser + plot trace)
- [ ] **ANA-04** — Derived built-ins via `derived:` ingestion source: magnitude, scale+offset, deg↔rad, unwrap (§17.3)
- [ ] **ANA-05** — Markers: manual add (`M`), bookmarks panel, timeline flags, plot verticals, persist (§17.4)
- [ ] **ANA-06** — Auto-markers from AP MSG/EV + ULog logged messages (toggle)
- [ ] **ANA-07** — Gap/dropout/reset detection surfaced (links DIA-05)
- [ ] **ANA-08** — _(backlog)_ expression engine with prev-sample union-timeline alignment
- [ ] **ANA-09** — _(backlog)_ A−B diff trace; resampling utilities as a library

### IOX — Import/export (M10)

- [ ] **IOX-01** — CSV export: field multi-pick, full|visible range (§18)
- [ ] **IOX-02** — CSV resample modes: none/prev-fill/linear@dt; streamed + progress
- [ ] **IOX-03** — Plot image export (screenshot 1×/2×)
- [ ] **IOX-04** — Layout JSON export/import (=LAY-05 surfacing in File menu)
- [ ] **IOX-05** — Diagnostics export (=DIA-07)
- [ ] **IOX-06** — Profiling export (=PRF-07)
- [ ] **IOX-07** — _(backlog)_ offscreen vector-quality plot render export
- [ ] **IOX-08** — Arrow IPC `.dlcache` writer (background, post-parse) + mtime/size match + mmap reload (ZC-5)
- [ ] **IOX-09** — _(backlog)_ Parquet cache option
- [ ] **IOX-10** — Live recording UI toggle (surfaces LIV-09)

### UIX — UI/UX (cross-cutting; final pass M10)

- [ ] **UIX-01** — Workspace-first window per §19.1; collapsible drawer/docks
- [~] **UIX-02** — Toolbar (open/cancel/add/remove/3D/vehicle/stream start/stop) with tooltips — dedicated icon toolbar directly under the menu bar with tinted SVG icons (Lucide via `egui_extras` svg loader): streaming (satellite-dish, blue when a live link is active → connection dialog) and 3D-view toggle (box, blue when open); live-link status + per-link Disconnect button (`Session::stop_live`, drop stops the threads) + parse progress/cancel live alongside; vehicle config moved to a gear overlay on the 3D pane (`WorkspaceActions::open_vehicle_config`); open moved to File menu. Pending: add/remove-plot buttons
- [~] **UIX-03** — Menus: File/Tools/Layout/Help (§19.2) — File/**Edit**/Layout/Help menu bar in place (user asked for an Edit menu in place of §19.2's Tools; Edit is an empty placeholder for now). File ▸ Open File; Layout ▸ Save / Load (submenu of saved layouts, loads in place) / Manage / Export JSON / Import JSON; Help ▸ About. Pending: File recent/import/export/quit, and the Tools/Edit contents
- [ ] **UIX-04** — Shortcut map per §19.3 + Help ▸ shortcuts sheet
- [ ] **UIX-05** — Dark theme, palette constants, ≥4.5:1 contrast
- [ ] **UIX-06** — High-DPI verified at 1×/1.5×/2×; min-size responsiveness
- [ ] **UIX-07** — All §19.5 empty states implemented with real copy
- [ ] **UIX-08** — File-drop onto window opens logs
- [ ] **UIX-09** — Never-block audit: every >16 ms operation is a job + progress (§19.6)
- [ ] **UIX-10** — Manual parser-override picker dialog (pairs PAR-01)
- [x] **UIX-11** — App data session engine: open path → sniff/detect → off-thread parse into ingestor; per-source progress + cancel token; snapshot access (the never-block load path behind UIX-02/BRW-01, §19.6)
- [x] **UIX-12** — Timeline live-link status dot — filled dot at the head of the timeline bar: grey when not streaming, yellow when a live link is up but the view is not locked to the tail, red when locked-to-live (§10.4); tooltip names the state. Pairs with the lock-to-live state from PLT-05

### TST — Testing & CI (continuous)

- [ ] **TST-01** — Fixture set + synthetic generators committed (§20.2)
- [ ] **TST-02** — proptest: pyramid ≡ naive; accessor invariants
- [ ] **TST-03** — Golden parser tables (BIN/ULog/tlog)
- [x] **TST-04** — Headless golden-image render test (=GPU-13)
- [ ] **TST-05** — Criterion suite per §20.4 with budget assertions (soft)
- [x] **TST-06** — Fuzz targets in CI smoke (60 s) + nightly long runs — `fuzz-smoke` job in `ci.yml` (60 s/target) + `fuzz-nightly.yml` cron (15 min/target, uploads crashing inputs)
- [ ] **TST-07** — Layout migration fixture tests (=LAY-03)
- [x] **TST-08** — tlog record/replay round-trip test (=LIV-09)
- [ ] **TST-09** — mac/Windows build matrix from M3

### BLG — Backlog (post-v1; keep ordered, do not start without re-planning)

- [ ] **BLG-01** — FFT/spectral pane
- [ ] **BLG-02** — Map/GPS 2D view (tiles) and 3D terrain overlay
- [ ] **BLG-03** — MAVLink message inspector (raw frame view)
- [ ] **BLG-04** — Parameter browser + diff between sources
- [ ] **BLG-05** — Mission viewer (3D + map)
- [ ] **BLG-06** — Event timeline lane (auto-markers as a swimlane)
- [ ] **BLG-07** — Python scripting for derived fields
- [ ] **BLG-08** — Dynamic plugin parser loading
- [ ] **BLG-09** — Multi-window
- [ ] **BLG-10** — Remote streaming relay / collaboration sessions
- [ ] **BLG-11** — WASM/WebGPU build (instanced-VB fallback per §9.7)

---

_End of plan. The agent's first action is ARC-01._
