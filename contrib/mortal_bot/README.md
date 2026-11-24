# Mortal bot for Akagi v3

[Mortal](https://github.com/Equim-chan/Mortal) (Equim-chan, AGPL-3.0-or-later)
wrapped in Akagi v3's external-bot protocol. 4-player only.

This folder holds the adapter (`bot.py`) and metadata. The installable zip
is assembled by `.github/workflows/mortal-bot.yml`, which adds, from
upstream Mortal at a pinned commit (`MORTAL_REF` in the zip):

| File | From |
|---|---|
| `model.py`, `engine.py` | `mortal/` — network and inference engine, unmodified |
| `libriichi.pyd` | `libriichi` built for Windows x64 / CPython 3.12 (Akagi's bundled Python) |
| `LICENSE` | upstream AGPL-3.0 text |

The checkpoint is **not** in the zip. Put it in the installed bot's folder as
`mortal.pth`, or point the **Checkpoint file** setting at it. It is loaded with
`torch.load(..., weights_only=True)`, which only rebuilds tensors and never
executes code from the file. Tested with the `mortal-hpc` 4p checkpoint
(`mortal_582500.pth`, SHA-256 `738e0d6e3c0ce9671629554ad39abd147d2ffbac676e80b194c83f2acc0fea20`).

## Install

1. Run the **Mortal bot (Windows)** workflow; download its artifact.
2. Akagi → **Bots** → install from ZIP → pick `mortal.zip` (name: `mortal`).
   The first install builds the Python environment (torch, ~150 MB).
3. Copy the checkpoint to `mjai_bot/mortal/mortal.pth` next to `akagi.exe`.
4. Activate it for 4-player.

## How it talks to Akagi

Each stdin line is a batch of mjai events whose last element is the decision
point. Every event is fed to `libriichi.mjai.Bot` (Mortal's own state
machine); only the last may act (`can_act=False` for the rest). Mortal's
reaction already carries `meta.q_values` / `meta.mask_bits`, which Akagi's
delay model reads; `bot.py` adds `meta.show` (top candidates with `prob` and
`type`) for the Bot Show tile. A bare `reach` is answered as mjai prescribes;
Akagi feeds back the declaration and reads the riichi discard.

Replaying a real ranked game (516 events, seat 2) through it gives one reply
per line and the same decisions whether events arrive one per line or in
Akagi-style batches; inference is ~20 ms per decision on CPU.
