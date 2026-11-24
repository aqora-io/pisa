# Search

Search finds Mathlib, Physlib and CSLib declarations from a description,
LaTeX, a Lean pattern or goal, or a partial or misspelled name. It is meant for
agents that know what they need but not what it is called. This page is for
those who call it: how it runs and ranks, where its descriptions come from,
the API, searching snapshots, what snapshot search sends to a model, and the
imports a hit needs. Loading, building and publishing an index is in
[Search indexes](search-index.md).

Its index is built from the same pinned project as the workers, and every
response carries that project's version as `version`, so the declarations it
returns exist in the matching worker pool.

## How it runs

The index lives in [Qdrant](https://qdrant.tech) (1.19.1 or later; Compose and
the chart run 1.20.0): a collection `pisa-<version>` with a point per
declaration, loaded from a bundle with `pisa-search load` (see
[Serving an index](search-index.md#serving-an-index)).

- **Settings.** A server searches when given `--qdrant URL --version VERSION`
  (`PISA_SEARCH_QDRANT`, `PISA_SEARCH_VERSION`), the version being the
  bundle's. `--embedder` (`PISA_SEARCH_EMBEDDER`) embeds queries for the dense
  lane; without one there is no dense lane. The others are in
  [Settings](search-index.md#settings).
- **Replicas.** The servers hold nothing, so any number of replicas serve one
  version. Each collection has one shard, as Qdrant's IDF is per shard; for
  availability, run Qdrant as a cluster and load with `--replication 2`.
- **One version.** Search serves one version. A request that names another is
  a 404 `not_found` that says which version search covers and that
  `GET /health` lists the pools. Agents on another pool find names with
  `#check`, `#print` and `exact?`.
- **Without search.** On a server without `--qdrant`, `/search`, `/resolve`
  and `/declarations/…` answer 404 `not_found` ("this server doesn't search:
  it has no --qdrant"), and `/health`'s `search` is `null`.
- **Starting.** A server opens its index in the background. Until Qdrant
  answers with the bundle, search requests answer 503 `unavailable` ("search
  isn't ready: …", with `Retry-After`), `/health`'s `search.ready` is false
  and `search.error` says why, and the rest of the API works.
- **When Qdrant fails.** If Qdrant doesn't answer, search requests are a 503
  and `/health`'s `search.qdrant` is false. `/health` waits for Qdrant for 2
  seconds, while it reads the database, so it still answers within about 2
  seconds. The server stays ready, as the rest of its API works and every
  replica shares the one Qdrant.

## How it ranks

Each of five lanes is a Qdrant query, and Qdrant fuses them with weighted
reciprocal rank fusion.

| Lane | What it matches |
| --- | --- |
| lexical | BM25 over names, descriptions, signatures and docstrings, as a sparse vector with a token per field and term |
| name | The name's trigrams, plus an exact match, which always comes first |
| structural | Constants the query names or implies, against each type's constants |
| conclusion | A goal's target or a `pattern`, against what each theorem concludes; off by default |
| dense | Qwen3-Embedding-8B vectors of every declaration, against the query's |

- **Structural.** A Lean word counts only when it names a declaration, so a
  goal's hypothesis names and variables don't; `consts` names constants
  exactly. LaTeX symbols, notation and quantum-information words (`|ψ⟩`, `ᴴ`,
  partial trace, fidelity) map to constants through `search/symbols.toml`. An
  entry there can also carry a hint, which comes back in the result's `hints`
  when its words are in the query, e.g. how to compare logarithms of different
  bases.
- **Conclusion.** A goal's target, after its `⊢`, or a `pattern`, against the
  constants of each theorem's conclusion, taken from its discrimination-tree
  key. Its default weight is 0, so it isn't asked: on the physics sets it
  lowered MRR, even for premises that conclude like their goal (see
  `search/eval/v4.36.0-rc2-1e541805-physics.md`).
- **Dense.** The vectors come from Scaleway's serverless API, which also
  embeds each query. Qwen3-Embedding-0.6B is a free, local alternative (TEI on
  CPU, or llama.cpp on a GPU) that retrieves somewhat worse (see
  `search/bench/2026-10-22/report.md`); the index must then be embedded with
  it too. A declaration is embedded as its kind and name, its description (or
  docstring) and its signature, in at most 2,000 characters: the description
  is cut so that the signature fits.

Qdrant's weights scale a lane's ranks rather than its scores: a weight below 1
makes a lane count only for its first few hits. The tuned defaults (`Weights`
in `crates/pisa-search/src/engine.rs`, tuned on the physics sets' dev splits
with MathlibQR as a guard) lean on the dense lane, with lexical and structural
matches lifting their top hits. PageRank over the dependency graph is a prior,
added after fusion, and a name query also favours names close to it by edit
distance.

Some declarations are left out:
- those that depend on `sorry`, unless `include_sorried` is set, which it is
  by default when `snapshots` is, so that agents find the skeleton's lemmas
  before they are proved;
- Lean-generated helpers (`….mk.injEq`) and metaprogramming internals
  (`Mathlib.Tactic.…`).

Any of them is still returned when a query names it exactly, a `sorry` one
flagged `uses_sorry`. Deprecated aliases and `to_additive` pairs fold into one
hit.

## Descriptions

A hit's `title` and `summary` come from a description of the declaration, in
words and in LaTeX. A description is a search aid; the signature says what
holds.

Mathlib declarations reuse the descriptions of
[LeanSearch v2](https://github.com/frenzymath/LeanSearch-v2), written for
Mathlib v4.28.0-rc1 and licensed under Apache-2.0. Their attribution is in
`search/NOTICE`, which a published index carries with every archive. One is
reused when the names match and the constants in its type are all among
ours.

`pisa-search informalize` writes the rest, with Claude Haiku 5.5 by default or
another Claude or OpenAI model:
- It works in dependency order, so each prompt shows descriptions of what the
  declaration uses.
- It caches by content hash, so after a library update only changed
  declarations cost anything.
- It checks LaTeX against KaTeX and checks that cited Lean names exist.
- It asks the same model, as a judge, which hypotheses a description leaves
  out, showing it the signature and the type with full names: premises such as
  `(hb : 1 < b)` or `n ≤ m →`, and typeclass assumptions that restrict the
  statement, such as finiteness, an order or nontriviality, but not one that
  only gives a type the structure the description names. One that leaves out
  `1 < b` claims more than the theorem proves.
- A declaration is judged if it has anything like a hypothesis: an instance
  argument, an argument whose name starts with `h` or whose type has a
  relation or a connective, or, for a theorem, an arrow in its type; variables
  alone don't count. A description that leaves some out is asked for again,
  once, with them named, and replaces the first only if the judge passes it.
- Its prompt has a glossary of how to read names many declarations use
  (`search/glossary/`): Mathlib's for every declaration, and Physlib's,
  QuantumInfo's terms included, for Physlib, PhyslibAlpha, QuantumInfo and
  snapshot declarations.

Readers flag a wrong description with
[`POST /declarations/{name}/flags`](#post-declarationsnameflags), and
[`pisa-search validate-descriptions`](search-index.md#validating-descriptions)
judges stored descriptions, flagged ones among them, as `informalize` judges
new ones.

## API

The types are in `crates/pisa-protocol/src/search.rs`, and the
[OpenAPI document](api.md#openapi) has them under the `search` tag. Errors
take the API's shape (see [Errors](api.md#errors)). Unknown names and empty
results are answers, not errors.

| Endpoint | What it does |
| --- | --- |
| `POST /search` | Runs up to 64 queries, a result each, in order |
| `POST /resolve` | Says whether names exist, with close names for those that don't |
| `GET /declarations/{name}` | Returns one declaration's details |
| `POST /declarations/{name}/flags` | Flags its description as wrong |

Each request may name a `version` (a query parameter for the two
`/declarations` endpoints): the index's version or its snapshots', or an alias
of one. Any other is a 404 `not_found`. A request that names none gets the
index, whatever the server's default version.

### POST /search

`{queries: [Query], version?}` → `{version, results: [{hits, text?,
warnings?, hints?, dense_pending?}]}`, at most 64 queries (more is a 400).

A query takes any of these fields. Fill as many as apply: each feeds
different lanes, and more fields mean better fusion.

| Field | Meaning |
| --- | --- |
| `text` | Free text, classified as natural language, LaTeX, Lean or a name |
| `description` | A description in words |
| `latex` | The statement in LaTeX, e.g. `S(\rho_{AB}) \le S(\rho_A) + S(\rho_B)` |
| `pattern` | A Lean expression or pattern, e.g. `Matrix.trace ?A = 1` |
| `goal` | A pretty-printed goal state: hypotheses and `⊢ target` |
| `name` | A declaration name, possibly partial or misspelled |
| `consts` | Constants the statement is about, by their exact names |
| `libraries` | Only these libraries, e.g. `["Physlib"]` |
| `kinds` | Only these kinds, e.g. `["theorem"]` |
| `limit` | Hits to return: 5 by default, at most 50 (a larger one is cut to 50) |
| `include_sorried` | Also return declarations that depend on `sorry`, flagged; by default only when `snapshots` is set |
| `snapshots` | Snapshots to search too: `"none"` (the default), `"all"` or `{"labels": {…}}`; see [Snapshots in search](#snapshots-in-search) |
| `format` | `"full"` (the default) or `"text"` |

```sh
curl -s localhost:8000/search -H 'content-type: application/json' -d '{
  "queries": [
    {"description": "trace of a density matrix is one", "libraries": ["Physlib"]},
    {"name": "Real.sqrt_mul_sefl", "limit": 3}
  ]
}'
```

A result has:
- `hits`, best first (empty with `format: "text"`);
- `text`, with `format: "text"`: each hit's `text`, separated by blank lines;
- `warnings`: what made the result worse than usual, e.g. a lane that failed;
- `hints`: advice the query's words earned (see [How it ranks](#how-it-ranks));
- `dense_pending`: the dense lane was left out because the query is still
  being embedded (below).

Each hit carries:
- `name`, `kind`, `library`, `module` and `signature`;
- for a definition, abbreviation or opaque, its `value`: what it is defined
  as, with notation, past the parameters the signature names, cut at 1,000
  characters (` …`). Recursive definitions have none, since Lean keeps only
  their compiled form, and a snapshot's is as its build printed it;
- a description's `title` and `summary`, when one exists: whole sentences, two
  at most within 240 characters. `description_source` says where they come
  from: `lsv2-v4.28.0-rc1` for LeanSearch v2's, the model that wrote it, or
  `docstring` when the summary is the docstring's;
- `flags`: `uses_sorry`; `informal`, for a Physlib informal result, stated but
  not proved; `deprecated_to`, the name to use instead; `additive_sibling`,
  the `to_additive` counterpart folded into the hit; `imports`, the header
  that loads it (see [Imports](#imports)); and `description_flags`, how many
  times readers flagged the description as it is now (one written again starts
  with none), which `text` mentions too;
- a `score`, comparable only within one query, and a `source_url`;
- `text`, about 150 tokens ready for a model's context: the signature (past
  400 characters, binders go from the middle so that the conclusion stays),
  the start of the value, and the summary;
- for a snapshot declaration, `snapshot_ids`, `status` and `verified` (see
  [Snapshots in search](#snapshots-in-search)).

A `libraries` or `kinds` filter matches case-insensitively; an unknown one is
a 400 that lists the known ones. `snapshots` on a server that doesn't search
snapshots is a 400, and so is `"libraries": ["PisaSnap"]` without
`snapshots`.

If the embedding API fails, or hasn't answered within 10 s, the query still
returns results from the other lanes and says so in the result's `warnings`.
A slow embedding goes on in the background, for up to 60 s, and the result has
`dense_pending`: asking again shortly includes the dense lane. Each server
keeps the last 1,024 query vectors, and a search for text that is already
being embedded waits for that request rather than sending another.

### POST /resolve

`{names, snapshots?, version?}` → `{version, results: [{name, exists,
deprecated_to?, imports?, suggestions}]}`, at most 64 names.

A result says whether the name exists, its replacement if it is deprecated,
and the header that loads it if the default imports don't. `suggestions` are
the closest existing names, best first, each `{name, kind, signature,
imports?}`; empty for an exact match. `/resolve` covers the bundle's names
only, unless it is sent `snapshots`, as a query's (by default `"none"`).

### GET /declarations/{name}

`?module=&version=` → `{version, declaration}`. The declaration has `name`,
`kind`, `library`, `module`, `signature`, `type_full` (the type with full
names and no notation), `value?`, `docstring?`, `description?` (`{title, text,
latex?, aliases, source}`), `flags`, `source_url?`, `dependencies` (what it
uses, in its statement and its proof or body) and `used_by` (what uses it,
most prominent first, at most 50).

With a snapshot hit's `module`, it is that snapshot's declaration, else the
bundle's. An unknown name is a 404 that names the closest existing one.

### POST /declarations/{name}/flags

`?module=&version=`, `{reason}` → `{version, description_flags}`: flags the
declaration's description as wrong, e.g. for leaving out a hypothesis, and
returns how many flags that description has now. The declaration is found as
`GET /declarations/{name}` finds it; one without a description is a 404.

A flag's `reason` is 1 to 2,000 characters. Flags are kept in the database the
servers share, each on the description it flagged, by a hash of its text, and
[`validate-descriptions --flagged`](search-index.md#validating-descriptions)
judges the flagged ones. A flag ignores `Pisa-Scope`: it is shared across
scopes and counted for every client. Each request adds one, and nothing
records who made it. Pisa has no authentication, so anyone who can reach the
search service can flag; review a `--flagged` run's targets with `--dry-run`
before paying for it.

### From a client

```python
from pisa_client import Client

client = Client("http://localhost:8000")
for hit in client.search(description="trace of a density matrix is one", libraries=["Physlib"])["hits"]:
    print(hit["text"])
client.resolve(["Real.sqrt_mul_sefl"])[0]["suggestions"][0]["name"]  # "Real.sqrt_mul_self"
client.get_declaration("Real.sqrt_mul_self")["used_by"]
client.flag_description("Real.logb_le_logb_of_le", "leaves out `1 < b`")  # the flags on it now
```

The Rust and TypeScript clients have the same methods (see
[Clients](clients.md)).

## Snapshots in search

The servers also search the declarations of [snapshots](snapshots.md), from
the database they share with the workers, ranked with the bundle's in every
lane. `PISA_SEARCH_SNAPSHOTS` (`--snapshots`) is true by default;
`--snapshots false` turns this off.

- **Scope.** A query's `snapshots` says which: `"none"` (the default),
  `"all"`, or `{"labels": {"run": "7"}}`, the modules of the snapshots that
  have every label, as `GET /snapshots?label=run:7&heads=true` matches them.
  The snapshots those import aren't included. Both cover heads only, the
  snapshots that nothing replaces (see [Replacing](snapshots.md#replacing-a-snapshot)): a
  skeleton drops out once its proof replaces it, and comes back if the proof
  is deleted. A head keeps every statement of what it replaces, so none is
  lost. Snapshot declarations are in the library `PisaSnap`, so adding
  `"libraries": ["PisaSnap"]` searches snapshots only.
- **Request scope.** Within a `Pisa-Scope` (see [Scopes](api.md#scopes)),
  `"all"` covers the snapshots with the scope's labels, and a label scope
  needs them too; labels that contradict the scope reach no snapshot.
  `GET /declarations/{name}` with `module=` is `not_found` for a module no
  snapshot of the scope is on.
- **Hits.** A hit's `module` is the snapshot's module, e.g. `PisaSnap.s1f…`,
  and `flags.imports` the header that loads it. `snapshot_ids` lists the
  scope's heads on that module, the snapshots to build on or replace, newest
  first, at most 10. `status` is the declaration's own, so a proved lemma next
  to a `sorry` is `proved`, and `verified` says that the module and everything
  it imports passed the kernel re-check. Names can repeat across snapshots,
  and each is a hit of its own.
- **Names.** `/resolve` covers the bundle only unless it is sent `snapshots`
  too, as a query is: then a name of a snapshot it reaches in the scope
  exists, with `imports` the header that loads its module, and suggestions
  come from those snapshots as well. `/declarations/{name}` without `module`
  covers the bundle only.
- **The leader.** One server at a time, holding a lease in the database (30 s,
  renewed every 10 s), indexes snapshots: it writes each module's declarations
  to Qdrant, and each snapshot to `pisa-<version>-entries`, which a label scope
  is resolved against. So a module can have any number of snapshots, but a
  label scope may cover at most 20,000 modules. When the leader goes, another
  server takes the lease, makes Qdrant match the catalog, and carries on;
  `/health`'s `search.leader` says which leads.
- **Freshness.** Workers export a module's declarations when they build it,
  and log every create, relabel, delete and re-check in the database. The
  leader reads that log every 2 s. A snapshot is searchable by name, signature
  and docstring seconds after it is created, with a vector of its docstring;
  its description, and a vector of that, follow when the model and the
  embedder answer. A module-system snapshot's declarations are exported as its
  module declares them, theorems as theorems and definitions with their values
  and docstrings, though importers see a theorem as an axiom and an unexposed
  definition without its value.
- **Descriptions.** With `PISA_SEARCH_DESCRIBE_MODEL` set to a Claude model
  (with `ANTHROPIC_API_KEY`) or an OpenAI one (with `OPENAI_API_KEY`), the
  leader describes every snapshot declaration as `informalize` describes the
  libraries, judge included. So all snapshot code goes to that provider (see
  [what it sends](#what-snapshot-search-sends-to-a-model)); with Claude Haiku
  5.5 a declaration costs about $0.0007, judging included, as the current
  index's descriptions did. Without a model, declarations are embedded from
  their docstrings. A failed description is tried 3 times in all; an
  authentication error pauses describing for 5 minutes, and a failed embedding
  pauses embedding for one. `PISA_SEARCH_DESCRIBE_CONCURRENCY` (8) bounds the
  requests in flight.
- **Vectors.** The embedder that embeds queries (`PISA_SEARCH_EMBEDDER`) also
  embeds each snapshot declaration's signature, docstring and description, so
  it must serve the model that embedded the bundle. A bundle without vectors
  gets no snapshot vectors either.
- **Storage.** Descriptions and vectors are kept in the database with the
  module, so a new leader writes the same points without calling either API,
  and Qdrant can be rebuilt from the database and the bundle. They are deleted
  with the module.
- **Version.** It searches the snapshots of the workers whose version is
  `PISA_SEARCH_VERSION`, the bundle's; `PISA_SEARCH_SNAPSHOT_VERSION` names
  another.

```python
hits = client.search(description="doubling adds a number to itself", snapshots={"labels": {"run": "7"}})["hits"]
hits[0]["module"], hits[0]["snapshot_ids"], hits[0]["status"]  # "PisaSnap.s1f…", ["v4.36.0-54500a67~9c0e…"], "proved"
```

## What snapshot search sends to a model

When `PISA_SEARCH_DESCRIBE_MODEL` is set, the search replica that leads the
index sends each snapshot declaration to that model, so code that a client
submitted leaves your deployment. With it unset, nothing goes to a model.
`GET /health` says which: `search.describe_model` is the model, or `null` if
no snapshot code is sent to one (also when `PISA_SEARCH_SNAPSHOTS` is false).

- **Who receives it.** A model name starting with `claude-` goes to Anthropic
  (`ANTHROPIC_API_KEY`, or `ANTHROPIC_BASE_URL` for another endpoint); any
  other name goes to OpenAI (`OPENAI_API_KEY`, or `OPENAI_BASE_URL`). The
  provider's own retention terms apply.
- **Per declaration**, one request describes it, with: its Lean name, kind and
  module name; its signature as Lean prints it (the name, arguments and type,
  not the proof or the value of a definition); its docstring, up to 2,000
  characters; and, for at most 30 declarations it uses outside the Init, Std
  and Lean libraries (including, for a definition, those its value uses), the
  name and the description, or if that isn't written yet the signature (300
  characters) and docstring (200 characters). Those it uses can be Mathlib or
  Physlib declarations, or other declarations of the same snapshot or of the
  snapshots it imports.
- **Judging.** A declaration with a hypothesis, such as an assumption, a
  premise or a typeclass argument, is sent again, to the same model, with its
  signature, its type printed with full names, and the description just
  written, to check that the description states the hypothesis. If it leaves
  one out, the declaration is described once more.
- **Not sent.** The text of proofs and of definition values (only the names of
  what a definition's value uses go out, as above), a module's `code` as a
  whole, its imports, comments, labels, scopes, the snapshot's id, and who
  created it.
- **Instructions.** Each request also carries the fixed instructions and
  glossary of `crates/pisa-search/src/informalize.rs`.
- **Another provider.** The embedder (`PISA_SEARCH_EMBEDDER`, Scaleway in
  Compose and the chart) gets each snapshot declaration's signature, docstring
  and description, whether or not a describe model is set, while snapshots are
  searched; it is a separate setting.
- **Turning it off.** Leave `PISA_SEARCH_DESCRIBE_MODEL` empty (Compose's
  `PISA_SEARCH_DESCRIBE_MODEL`, the chart's `search.describeModel`), or set
  `PISA_SEARCH_SNAPSHOTS` to false (the chart's `search.snapshots`); restart
  the servers, and check `/health`. If you only emptied the model,
  descriptions already written stay in the database and keep being searched;
  with `PISA_SEARCH_SNAPSHOTS` false, snapshots aren't searched at all.

## Imports

Most of the index loads with `import Mathlib` and `import Physlib`. Mathlib,
Physlib, QuantumInfo (density matrices, entropies, channels), PhyslibAlpha
(Physlib's lightly reviewed, often AI-written contributions) and CSLib
(automata, computability, semantics, algorithms) import together, so the index
is exported from one environment. A QuantumInfo, PhyslibAlpha or CSLib hit
carries the header that loads it, one import a line, in `flags.imports`
(`import Mathlib`, `import Physlib` and `import QuantumInfo` or `import
PhyslibAlpha`; `import Mathlib` and `import Cslib` for CSLib) and in its
`text`, and `/resolve` gives it for a name and each suggestion.

PhyslibAlpha is searched by default; `libraries` narrows a search to the
reviewed ones. A filter names libraries as the index's manifest does; the
current index has `Mathlib`, `Physlib`, `PhyslibAlpha`, `QuantumInfo`,
`Cslib`, `Batteries`, `Init` and `Std`, and snapshot declarations are in
`PisaSnap`.
