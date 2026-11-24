# Third-party notices

ARIA itself is under [PolyForm Noncommercial 1.0.0](LICENSE). What follows belongs to its authors and keeps its own
licence. Licences read at their source on 2026-11-24.

## Speech synthesis

| Work | Author | Licence | In ARIA |
|---|---|---|---|
| [phonon-40m-fr](https://huggingface.co/gradium/phonon-40m-fr) | Gradium | **CC BY-NC 4.0** | downloaded from our own release, unmodified ([notice](app/resources/phonon/phonon-40m-fr/NOTICE.txt)) |
| the `-en` and `-multilingual` 40M models | Gradium | **CC BY-NC 4.0** | downloaded, gated |
| [phonon-31m-en](https://huggingface.co/gradium/phonon-31m-en) | Gradium | Apache-2.0 | downloaded |
| [phonon-voices](https://huggingface.co/gradium/phonon-voices) | Gradium | Apache-2.0 | shipped, unmodified (`app/resources/voices/`) |
| [ptts](https://github.com/gradium-ai/xn-ptts) | Gradium | MIT or Apache-2.0 | linked |
| [xn](https://github.com/LaurentMazare/xn) | Laurent Mazare and contributors | MIT or Apache-2.0 | linked |
| [xn-gemm](https://github.com/gradium-ai/gemm) | sarah, Gradium | MIT | linked |
| [Pocket TTS](https://github.com/kyutai-labs/pocket-tts), code, through [a fork](https://github.com/Coopaguard/pocket-tts) | Kyutai | MIT | linked; not offered on screen |
| [Pocket TTS](https://huggingface.co/kyutai/pocket-tts), weights | Kyutai | CC BY 4.0 | not downloaded by default |
| Kyutai TTS voices | Kyutai, and per-voice authors | per voice | not shipped |

## Listening

| Work | Author | Licence | In ARIA |
|---|---|---|---|
| [Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) | NVIDIA | CC BY 4.0 | downloaded |
| [ONNX export of Parakeet](https://huggingface.co/altunenes/parakeet-rs) (format changed, weights NVIDIA's) | altunenes | as NVIDIA's | downloaded |
| [parakeet-rs](https://github.com/altunenes/parakeet-rs) | altunenes | MIT or Apache-2.0 | linked |
| [Smart Turn v3](https://huggingface.co/pipecat-ai/smart-turn-v3) | Pipecat (Daily) | BSD-2-Clause | downloaded |
| [openWakeWord](https://github.com/dscripka/openWakeWord) feature models | David Scripka; speech embedding by Google | see [`NOTICE.txt`](app/resources/wake/NOTICE.txt) — treated as non-commercial | shipped, unmodified |
| [handy-recorder](https://github.com/handy-computer/handy-recorder), handy-cpal | Handy | MIT, Apache-2.0 | linked |

## Memory, inference, optional models

| Work | Author | Licence | In ARIA |
|---|---|---|---|
| [multilingual-e5-base](https://huggingface.co/intfloat/multilingual-e5-base) | intfloat | MIT | downloaded |
| [fastembed](https://github.com/Anush008/fastembed-rs) | Anush008 | Apache-2.0 | linked |
| [ONNX Runtime](https://github.com/microsoft/onnxruntime) | Microsoft | MIT | shipped |
| DirectML | Microsoft | Microsoft Software License Terms | shipped |
| CUDA and cuDNN redistributables | NVIDIA | NVIDIA licence terms | downloaded when enabled |
| [Gemma 4 E4B](https://huggingface.co/google/gemma-4-E4B-it) | Google | Apache-2.0 | downloaded when chosen |
| [Voxtral Mini 3B](https://huggingface.co/mistralai/Voxtral-Mini-3B-2507) | Mistral AI | Apache-2.0 | downloaded when chosen |
| Claude Code | Anthropic | Anthropic terms | not included, installed by the user |

The other Rust and JavaScript dependencies are named, with their versions, in `Cargo.lock` and the
two `package-lock.json`. All are under permissive licences, or MPL-2.0 and unmodified (Symphonia,
cssparser, selectors and others), whose sources are on crates.io.
