# Licences of the Phonon voices

| | Licence | In ARIA |
|---|---|---|
| `phonon-40m-fr` | CC BY-NC 4.0, gated on Hugging Face | downloaded from our release `models-1`, with its [notice](../../app/resources/phonon/phonon-40m-fr/NOTICE.txt) |
| `phonon-40m-multilingual`, `phonon-40m-en` | CC BY-NC 4.0, gated | downloaded with the user's token |
| `phonon-31m-en` | Apache-2.0 | downloaded |
| the 102 voices (`phonon-voices`) | Apache-2.0 | shipped |

We hand out the French model ourselves so that nobody creates a token. CC BY-NC allows it: free of charge, with attribution,
unmodified. The gate asks to share one's Hugging Face profile, and the card,
read signed in on 2026-11-25, adds no term to the licence. The gate is still their way of knowing who uses the model.

- [ ] Tell Gradium, ask whether handing it out without the gate suits them, and keep the answer.
- [ ] Show in the settings the licence and the author of the model and of the voice in force.
- [ ] Show the CC BY-NC notice once at first start, and say that the voice is synthetic (their "responsible use").
- [ ] Add the multilingual model to a `models-2` (122 MB), or keep the token for it. Until then its 87 voices need one.
- [ ] A voice carries its own licence in the catalogue, before any other source of voices is added
      ([custom voices](custom-voices.md), consent of the speaker included).
- [ ] The card of the model puts "the voices in this repository" under CC BY-NC, and `phonon-voices` says Apache-2.0:
      ARIA takes its voices from the second only, never `default-voice.safetensors`, for the French model.
- [ ] Commercial use would need another French model, or an agreement with Gradium.
