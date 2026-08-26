# Architecture

## Data path

```text
Rust source and cursor
        |
        v
custom rustc -> bounded ownership artifacts and compiler diagnostics
        |
        v
custom rust-analyzer -> versioned LSP requests and notifications
        |
        v
Zed project bridge -> schema, size, range, status, and ID validation
        |
        v
Rust Workbench panel and inline annotations
```

The UI does not parse generated C or invent compiler state. It requests
compiler-backed facts through four custom LSP operations: file problems, the
selected ownership model, a workspace guide, and repair actions. Candidate
repairs stay non-applicable until rustc validation succeeds.

## Consistency rules

Every asynchronous panel request is tied to source hash, selected problem,
selection generation, and analyzer generation. Late responses are discarded.
An analyzer restart invalidates pending identities. A transient transport error
can keep displaying the last compiler facts only when their source hash still
matches the active buffer.

The project bridge rejects unsupported schemas, inverted ranges, duplicate or
empty IDs, unknown statuses, excessive collection sizes, and oversized text.
This boundary prevents malformed analyzer data from becoming unbounded GPUI
elements or invalid editor ranges.

## Visual model

The visualization derives a beginner-facing topology from compiler facts and a
bounded fallback catalog. It explicitly separates stack bindings, handles,
wrapper layers, heap allocations, borrow gates, vector buffers, async saved
locals, and trait-object vtable metadata. Large graphs and issue lists use hard
render limits. Empty or incomplete compiler facts still produce a nonblank
three-step contract, rejected operation, and repair diagram.

## Process isolation

The launcher allocates one writable profile per process using `flock`.
`RUST_WORKBENCH_INSTANCE_ID` also namespaces rust-analyzer ownership artifacts
and repair-validation targets. Independent processes can analyze the same
workspace without sharing model files or Cargo output.
