"""Mortal (Equim-chan/Mortal) as an Akagi v3 mjai bot.

Akagi v3 protocol: each stdin line is a JSON *array* of mjai events (the
last one is the decision point); reply with exactly one reaction per line
on stdout. Mortal's own `libriichi.mjai.Bot` takes one event at a time and
keeps the player state, so every event of a batch is fed through it and
only the last one is allowed to act (`can_act=False` for the rest): the
earlier ones are history, and acting on them would answer windows that
have already closed.

The reaction Mortal produces already carries `meta.q_values` (one per legal
action, in action-index order) and `meta.mask_bits`; Akagi's delay model
softmaxes those itself. On top, `meta.show` lists the top candidates for the
Bot Show tile, each with `prob` and `type` so delay scripts can read them.

Files next to this script: `model.py` / `engine.py` (upstream Mortal,
AGPL-3.0-or-later), `libriichi.pyd` (upstream libriichi, built for this
platform and Python), and the checkpoint (`model_path` setting, default
`mortal.pth`). The checkpoint is loaded with `weights_only=True`, which only
rebuilds tensors and never runs code from the file.
"""

import json
import os
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

NOTIFY_PREFIX = "@@AKAGI_NOTIFY@@ "


def log(msg: str) -> None:
    sys.stderr.write(f"[mortal] {msg}\n")
    sys.stderr.flush()


def notify(level: str, title: str, body: str | None = None, *, id: str | None = None) -> None:
    payload = {"level": level, "title": title}
    if body is not None:
        payload["body"] = body
    if id is not None:
        payload["id"] = id
    sys.stderr.write(NOTIFY_PREFIX + json.dumps(payload, separators=(",", ":")) + "\n")
    sys.stderr.flush()


def load_settings() -> dict:
    path = os.environ.get("AKAGI_BOT_CONFIG")
    if not path:
        return {}
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except Exception as e:  # a broken settings file must not stop play
        log(f"could not read settings ({e}); using defaults")
        return {}


def initial_seat() -> int:
    for raw in (sys.argv[1] if len(sys.argv) > 1 else None, os.environ.get("AKAGI_PLAYER_ID")):
        try:
            if raw is not None and int(raw) in range(4):
                return int(raw)
        except ValueError:
            pass
    return 0


# ---------- action index → HUD row ----------
# Mortal v4 action space (46): 0..33 tiles 1m..9m 1p..9p 1s..9s E S W N P F C,
# 34..36 red fives (5mr 5pr 5sr), then riichi, chi low/mid/high, pon, kan,
# hora, ryukyoku, pass.

TILES = (
    [f"{n}m" for n in range(1, 10)]
    + [f"{n}p" for n in range(1, 10)]
    + [f"{n}s" for n in range(1, 10)]
    + ["E", "S", "W", "N", "P", "F", "C", "5mr", "5pr", "5sr"]
)
NON_DISCARD = {
    37: ("Riichi", "reach"),
    38: ("Chi (low)", "chi"),
    39: ("Chi (mid)", "chi"),
    40: ("Chi (high)", "chi"),
    41: ("Pon", "pon"),
    42: ("Kan", None),  # daiminkan / ankan / kakan: the index does not say which
    43: ("Win", "hora"),
    44: ("Abortive draw", "ryukyoku"),
    45: ("Pass", "none"),
}


def softmax(xs: list[float]) -> list[float]:
    if not xs:
        return []
    m = max(xs)
    es = [pow(2.718281828459045, x - m) for x in xs]
    s = sum(es)
    return [e / s for e in es]


def show_from_meta(meta: dict, top_n: int) -> dict | None:
    """`meta.show` from Mortal's compact q-values: one value per set bit of
    `mask_bits`, in ascending action-index order."""
    q = meta.get("q_values")
    mask = meta.get("mask_bits")
    if not isinstance(q, list) or not isinstance(mask, int) or not q:
        return None
    indices = [i for i in range(46) if mask >> i & 1]
    if len(indices) != len(q):
        return None
    probs = softmax([float(v) for v in q])
    rows = sorted(zip(indices, probs), key=lambda r: r[1], reverse=True)[:top_n]
    items = []
    for idx, p in rows:
        if idx < 37:
            item = {"label": "Discard", "pais": [TILES[idx]], "type": "dahai"}
        else:
            label, typ = NON_DISCARD.get(idx, (f"Action {idx}", None))
            item = {"label": label}
            if typ:
                item["type"] = typ
        item["value"] = f"{p * 100:.1f}%"
        item["prob"] = p
        items.append(item)
    return {"title": "Mortal", "items": items}


# ---------- play style ----------
# Applied where Mortal picks its action (`MortalEngine._react_batch`), so
# libriichi still turns the chosen index into the mjai event — chi/pon tile
# choice and tsumogiri stay Mortal's own bookkeeping.

RIICHI = 37
CALLS = (38, 39, 40, 41)  # chi low/mid/high, pon (not kan)
NEVER_SWAP = (43, 44)  # a win or an abortive draw is never traded


def restyle(q: list[float], mask: list[bool], style: dict, rng) -> tuple[int, list[float], bool]:
    """Pick an action from one decision's 46 Q-values.

    1. Aggression: add `riichi_bias` to riichi and `call_bias` to chi/pon
       (Q units; the HUD's percentages are a softmax of these).
    2. Second choice: when the runner-up's share is at least `threshold`,
       take it with probability `rate * p2 / p1` — near-ties often,
       lopsided calls rarely. Never when either is a win or abortive draw.

    Returns (action, the biased Q-values, whether it is the top choice)."""
    q = list(q)
    legal = [i for i in range(len(q)) if mask[i]]
    if style["riichi_bias"] and RIICHI in legal:
        q[RIICHI] += style["riichi_bias"]
    if style["call_bias"]:
        for i in CALLS:
            if i in legal:
                q[i] += style["call_bias"]
    ranked = sorted(legal, key=lambda i: q[i], reverse=True)
    best = ranked[0]
    if len(ranked) < 2 or style["rate"] <= 0:
        return best, q, True
    second = ranked[1]
    if best in NEVER_SWAP or second in NEVER_SWAP:
        return best, q, True
    top = max(q[i] for i in legal)
    weights = {i: pow(2.718281828459045, q[i] - top) for i in legal}
    total = sum(weights.values())
    p1, p2 = weights[best] / total, weights[second] / total
    if p2 >= style["threshold"] and rng.random() < style["rate"] * p2 / p1:
        return second, q, False
    return best, q, True


def style_from(settings: dict) -> dict:
    def num(key, default, lo, hi):
        try:
            return min(hi, max(lo, float(settings.get(key, default))))
        except (TypeError, ValueError):
            return default
    aggression = num("aggression", 0.0, 0.0, 1.0)
    return {
        # Calibrated on a real game: the median Q gap between the top two
        # options is ~2, a coin-flip riichi or call ~0.5; full aggression
        # (+0.6 / +0.4) tips borderline ones, never clear folds.
        "riichi_bias": 0.6 * aggression,
        "call_bias": 0.4 * aggression,
        "threshold": num("second_choice_threshold", 0.18, 0.05, 0.5),
        "rate": num("second_choice_rate", 0.0, 0.0, 1.0),
    }


# ---------- model ----------

def build_engine(model_path: Path, style: dict):
    import random
    import torch
    from engine import MortalEngine
    from model import Brain, DQN

    class StyledEngine(MortalEngine):
        """Mortal's engine with `restyle` deciding instead of argmax."""

        def _react_batch(self, obs, masks, invisible_obs):
            actions, q_out, mask_out, greedy = super()._react_batch(obs, masks, invisible_obs)
            for b in range(len(actions)):
                a, q, g = restyle(q_out[b], mask_out[b], style, rng)
                actions[b], q_out[b], greedy[b] = a, q, g
            return actions, q_out, mask_out, greedy

    rng = random.Random()

    torch.set_num_threads(max(1, min(4, os.cpu_count() or 1)))
    state = torch.load(str(model_path), weights_only=True, map_location=torch.device("cpu"))
    cfg = state["config"]
    version = cfg["control"].get("version", 1)
    num_blocks = cfg["resnet"]["num_blocks"]
    conv_channels = cfg["resnet"]["conv_channels"]
    brain = Brain(version=version, num_blocks=num_blocks, conv_channels=conv_channels).eval()
    dqn = DQN(version=version).eval()
    brain.load_state_dict(state["mortal"])
    dqn.load_state_dict(state["current_dqn"])
    engine = StyledEngine(
        brain,
        dqn,
        version=version,
        is_oracle=False,
        device=torch.device("cpu"),
        enable_amp=False,
        enable_quick_eval=True,
        enable_rule_based_agari_guard=True,
        name="mortal",
    )
    return engine, f"v{version} {num_blocks}x{conv_channels}"


def main() -> None:
    # Akagi writes UTF-8; Python on Windows reads pipes in the locale's code
    # page (GBK on Chinese systems) unless told otherwise, which turns
    # non-ASCII player names into lone surrogates that libriichi's JSON
    # parser rejects.
    for stream in (sys.stdin, sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError):
            pass
    settings = load_settings()
    top_n = int(settings.get("show_top_n", 3) or 3)
    model_path = Path(settings.get("model_path") or "mortal.pth")
    if not model_path.is_absolute():
        model_path = HERE / model_path
    if not model_path.is_file():
        notify(
            "error",
            "Mortal: model file not found",
            f"Put the checkpoint at {model_path} (or set its path in the bot's settings).",
            id="mortal-model",
        )
        log(f"model file not found: {model_path}")
        sys.exit(1)

    t0 = time.monotonic()
    from libriichi.mjai import Bot

    style = style_from(settings)
    engine, desc = build_engine(model_path, style)
    log(
        f"loaded {model_path.name} ({desc}) in {time.monotonic() - t0:.1f}s; "
        f"style: riichi {style['riichi_bias']:+.2f}, calls {style['call_bias']:+.2f}, "
        f"second choice >= {style['threshold']:.0%} at rate {style['rate']:.2f}"
    )

    seat = initial_seat()
    bot = None

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        events = json.loads(line)
        if isinstance(events, dict):
            events = [events]
        reaction = None
        game_over = False
        for i, ev in enumerate(events):
            typ = ev.get("type")
            if typ == "start_game":
                seat = int(ev.get("id", seat))
                bot = Bot(engine, seat)
            elif bot is None:
                # Joined mid-stream without a start_game: Mortal cannot
                # rebuild its state from here, so it only watches.
                continue
            if typ == "end_game":
                game_over = True
            if typ == "start_game":
                # Mortal never reads the names (the field defaults), and a
                # name is the one free-text field in the stream: keep it out.
                ev = {k: v for k, v in ev.items() if k != "names"}
            last = i == len(events) - 1
            out = bot.react(json.dumps(ev, separators=(",", ":")), can_act=last)
            if last and out:
                reaction = json.loads(out)

        if reaction is None:
            reaction = {"type": "none"}
        else:
            meta = reaction.get("meta")
            if isinstance(meta, dict):
                show = show_from_meta(meta, top_n)
                if show:
                    meta["show"] = show
        sys.stdout.write(json.dumps(reaction, separators=(",", ":")) + "\n")
        sys.stdout.flush()
        if game_over:
            break


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
