# Embedding and informalization benchmark, 2026-10-22

Environment `v4.35.0-da683984` (Mathlib + Physlib, 418,154 declarations). Total spend: about $8 in model APIs plus about €0.35 for Scaleway embeddings, under the $30 cap.

## Summary

- **Embeddings:** the laptop's Radeon 780M iGPU embeds 13× faster than its CPU, with the same vectors. On a 20k-declaration slice, Qwen3-Embedding-8B (Scaleway serverless) retrieves clearly better than 0.6B (dense R@10 0.795 against 0.690), but each query then costs a 0.7–1.7 s API round trip.
- **Informalization:** Claude Haiku 5.5 is the best value. Its descriptions retrieve within 0.03 R@10 of the best model (Sonnet 5.5) at 1/19 of the cost. GPT-6 Luna is cheaper still and close behind. Opus 5.5 was no better than Haiku and returned empty descriptions for 8% of declarations. GPT-6.1 Sol retrieved worst.

## A. Embeddings

**Throughput** is measured on the same 500 Mathlib document views, averaging 154 tokens; the full corpus averages 142.

**Agreement** is the mean cosine with the CPU vectors for the same texts. The minimum was 0.9998 for both iGPU backends, so their vectors are interchangeable with the CPU's.

| Backend | Model | Hardware | Docs/s (500 views) | Docs/s (larger runs) | Agreement | Cost |
| --- | --- | --- | ---: | ---: | ---: | --- |
| TEI `cpu-1.9` | Qwen3-Embedding-0.6B | Ryzen 9 7940HS, 8 cores | 1.3 | 1.5–2.7 (Physlib views, earlier run) | reference | local |
| llama.cpp `server-vulkan`, f16 GGUF | Qwen3-Embedding-0.6B | Radeon 780M iGPU | 12.8 | — | 0.9999 | local |
| llama.cpp `server-rocm`, f16 GGUF, `HSA_OVERRIDE_GFX_VERSION=11.0.0` | Qwen3-Embedding-0.6B | Radeon 780M iGPU | 16.1 | 16.5 (18,940 slice views), 21.9 (21,383 Physlib views) | 0.9999 | local |
| Scaleway Generative APIs (serverless), one request at a time | Qwen3-Embedding-8B | API | 10.0 | 13.3 (19,500 slice views) | different model | €0.10/M tokens, about €0.014 per 1k views |
| TEI on a Scaleway L4 or L40S instance | — | — | not run | — | — | L4-1-24G and L40S-1-48G were out of stock in fr-par-2 and pl-waw-2 all day (instances created, refused at start, deleted) |

**Embedding the whole corpus** (418k views, about 59M tokens):

| Route | Time | Cost |
| --- | --- | --- |
| CPU | about 90 hours | local |
| iGPU with ROCm | about 7 hours | local |
| Scaleway serverless 8B | about 9 hours, one request at a time (parallel requests would cut this) | about €5.90 |

**Retrieval** uses the MathlibQR queries (922, as free text). The pool is a 20,000-declaration slice: the 195 targets, every other declaration in their modules, and random Mathlib declarations. Only the slice has vectors.

| Embedding model | Dense alone: R@1 | R@5 | R@10 | R@50 | MRR |
| --- | ---: | ---: | ---: | ---: | ---: |
| Qwen3-Embedding-0.6B | 0.267 | 0.584 | 0.690 | 0.787 | 0.401 |
| Qwen3-Embedding-8B | **0.310** | **0.678** | **0.795** | **0.885** | **0.466** |
| (lexical lane, for reference) | 0.152 | 0.355 | 0.425 | 0.552 | 0.244 |

Dense R@10 by query style:

| Model | Lean | LaTeX | Natural | Slogan | Nickname |
| --- | ---: | ---: | ---: | ---: | ---: |
| Qwen3-Embedding-0.6B | 0.608 | 0.672 | 0.722 | 0.724 | 0.768 |
| Qwen3-Embedding-8B | **0.784** | **0.785** | **0.804** | **0.812** | **0.808** |

Caveats:
- A 20k pool is easier than the full 418k index, so absolute numbers are optimistic. The comparison between models uses the same pool.
- Document text (including LeanSearch v2 descriptions) and the query instruction are the same for both models.

## B. Informalization models

All five models got the same prompts, with effort `low` for every model. Luna also offers `none`, which wasn't tested.

**MathlibQR retrieval.** Each model described the 186 MathlibQR targets that need a description; the other 9 are instances and the like. Its descriptions replaced LeanSearch v2's for those targets only, and we measured fused retrieval over the full index (922 queries, no dense lane).

**Physlib.** Each model described the same 107-declaration Physlib sample (seed 1).
- The checks are those of the pilot.
- Discrimination asks whether a description picks out its declaration from it and its 3 nearest neighbours, with 20 items per model, judged by both Haiku 5.5 and Luna.

| Model | $/description | Seconds (186 / 107 at concurrency 8) | MathlibQR R@10 | MathlibQR MRR | Physlib: all checks | Physlib: has formula | Discrimination (Haiku / Luna judge) | Empty answers |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Claude Haiku 5.5 | 0.00031–0.00042 | 58 / 36 | 0.625 | 0.376 | 94% | 71% | 16/20, 18/20 | 0 |
| Claude Sonnet 5.5 | 0.0059–0.0078 | 84 / 57 | **0.658** | **0.394** | 89% | 80% | 16/20, 16/20 | 0 |
| Claude Opus 5.5 | 0.012–0.017 | 99 / 66 | 0.629 | 0.377 | 99% (of 98) | 58% | 16/20, 17/20 | 14/186 (4 after one retry), 9/107 |
| GPT-6 Luna | 0.00021–0.00023 | 96 / 51 | 0.611 | 0.365 | 91% | 93% | 16/20, 17/20 | 0 |
| GPT-6.1 Sol | 0.0039–0.0046 | 151 / 122 | 0.570 | 0.333 | 90% | 89% | 19/20, 18/20 | 0 |
| LeanSearch v2 descriptions | — | — | 0.424 | 0.240 | — | — | — | — |
| No descriptions | — | — | 0.331 | 0.173 | — | — | — | — |

MathlibQR R@10 by query style (fused):

| Descriptions | Lean | LaTeX | Natural | Slogan | Nickname |
| --- | ---: | ---: | ---: | ---: | ---: |
| Claude Haiku 5.5 | 0.644 | 0.518 | 0.655 | 0.615 | 0.800 |
| Claude Sonnet 5.5 | 0.686 | 0.626 | 0.655 | 0.630 | 0.784 |
| Claude Opus 5.5 | 0.655 | 0.579 | 0.644 | 0.583 | 0.768 |
| GPT-6 Luna | 0.649 | 0.590 | 0.582 | 0.573 | 0.744 |
| GPT-6.1 Sol | 0.593 | 0.513 | 0.557 | 0.557 | 0.720 |
| LeanSearch v2 | 0.423 | 0.287 | 0.438 | 0.448 | 0.624 |
| None | 0.423 | 0.205 | 0.294 | 0.307 | 0.512 |

Caveats:
- **The gain over LeanSearch v2 is inflated.** Only the targets were re-described; every competing declaration kept LeanSearch v2's text, so a different description style alone can make targets stand out. The ranking between models is the fair comparison, since all faced the same setup.
- **Discrimination doesn't separate the models.** With 20 items, every model and judge pair scored 16–19.
- **Opus's empty answers are intermittent.** Opus 5.5 returned valid JSON with an empty `statement_nl` for 7.5% of the targets and 8.4% of the Physlib sample. Sent singly, a failing declaration (`Polynomial.separable_def`) was described correctly 3 times out of 3. The pipeline reports these and leaves them for the next run.
- **"All checks" differs mostly by LaTeX.** Models that write more formulas (Luna, Sol, Sonnet) trip the KaTeX allowlist more often. Opus writes fewest formulas and passes most.

**Full-run cost** for the 127,305 declarations still without a description, at the Physlib $/description:

| Model | All undescribed | Physlib only (10,488) |
| --- | ---: | ---: |
| Claude Haiku 5.5 | $53 ($27 with Message Batches) | $4.40 |
| GPT-6 Luna | $29 | $2.40 |
| GPT-6.1 Sol | $590 | $49 |
| Claude Sonnet 5.5 | $998 ($499 with Message Batches) | $82 |
| Claude Opus 5.5 | $2,106 ($1,053 with Message Batches) | $173 |

## Recommendation

**Embeddings:** switch the dense lane to Qwen3-Embedding-8B through Scaleway's serverless API.
- **Quality:** it beats 0.6B by about 0.1 R@10, on every query style.
- **Corpus cost:** about €6 to embed all 418k declarations. Sending several requests at a time should cut the 9 hours of a single stream.
- **Query cost:** each search embeds the query through the same API, adding a round trip (0.7–1.7 s measured from here) and a negligible cost.
- **Offline alternative:** 0.6B on the iGPU with ROCm, about 7 hours and free, for when no external dependency is wanted. Interchangeable with the CPU vectors, it is the fastest local option and 13× the CPU.
- **Not measured:** 8B on the iGPU; it would fit in the 60 GB of shared memory.
- **Scaleway GPU instances:** not needed for this.

**Informalization:** Claude Haiku 5.5 at effort low for the full run. It is within 0.03 R@10 of Sonnet 5.5 at 1/19 of the price. It returned no empty answers here (2 of 508 in yesterday's pilot). GPT-6 Luna is a close, cheaper alternative. Before committing to a provider, it would be worth a second, larger retrieval test that re-describes the competitors too, which removes the style bias.

## Reproducing

Scripts and logs are in `search/work/` (gitignored):
- name lists: `scripts/names.py`;
- per-model stores, bundles and eval reports: under `stores/`, `bundles/` and `eval/`.

The serving setup used for the iGPU:

```sh
docker run -d --device /dev/kfd --device /dev/dri --group-add video --group-add render \
  -e HSA_OVERRIDE_GFX_VERSION=11.0.0 -p 127.0.0.1:8093:8080 -v ~/.cache/llama.cpp:/root/.cache/llama.cpp \
  ghcr.io/ggml-org/llama.cpp:server-rocm -hf Qwen/Qwen3-Embedding-0.6B-GGUF:F16 \
  --embedding --pooling last -ngl 99 -c 8192 -b 8192 -ub 8192 --host 0.0.0.0 --port 8080
pisa-search embed ... --embedder "openai+http://localhost:8093/v1?model=qwen3-embedding-0.6b" --label Qwen/Qwen3-Embedding-0.6B
```
