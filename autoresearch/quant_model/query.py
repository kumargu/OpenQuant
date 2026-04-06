"""
Query the trained quant GPT model.
Usage: python query.py "your prompt here"

Loads the model checkpoint and generates text completions.
The model architecture is reconstructed from the checkpoint config.
"""

import os
import sys
import pickle
import torch
import torch.nn.functional as F

CACHE_DIR = os.path.join(os.path.expanduser("~"), ".cache", "autoresearch-quant")
TOKENIZER_PATH = os.path.join(CACHE_DIR, "tokenizer", "tokenizer.pkl")
CHECKPOINT_PATH = os.path.join(CACHE_DIR, "model.pt")
device = "mps" if torch.backends.mps.is_available() else "cpu"


def load_tokenizer():
    with open(TOKENIZER_PATH, "rb") as f:
        return pickle.load(f)


def load_model():
    """Load model from checkpoint. Reconstructs architecture from saved config."""
    ckpt = torch.load(CHECKPOINT_PATH, map_location=device, weights_only=False)
    cd = ckpt["config"]

    # Import model class from train.py without triggering training.
    # We patch sys.modules to prevent prepare.py side effects.
    import types
    fake_prepare = types.ModuleType("prepare")
    fake_prepare.MAX_SEQ_LEN = cd["sequence_len"]
    fake_prepare.TIME_BUDGET = 300
    fake_prepare.Tokenizer = None
    fake_prepare.make_dataloader = None
    fake_prepare.evaluate_bpb = None
    sys.modules["prepare"] = fake_prepare

    # Now we can import GPT and GPTConfig from train.py
    # But train.py has module-level code that runs. We need to intercept.
    # Safest: just exec the class definitions.
    train_path = os.path.join(os.path.dirname(__file__), "train.py")
    with open(train_path) as f:
        source = f.read()

    # Extract from "class GPTConfig" up to but not including "t_start"
    import re
    # Extract model class definitions only (GPTConfig through GPT class)
    # Then extract build_model_config separately
    match_classes = re.search(r'(@dataclass\nclass GPTConfig.*?class GPT\(nn\.Module\):.*?return logits)', source, re.DOTALL)
    match_builder = re.search(r'(def build_model_config\(depth\):.*?window_pattern=WINDOW_PATTERN,\n    \))', source, re.DOTALL)
    if not match_classes or not match_builder:
        raise RuntimeError("Could not extract model classes from train.py")

    model_code = match_classes.group(1) + "\n\n" + match_builder.group(1)
    # Prepend needed imports and constants
    preamble = """
import torch
import torch.nn as nn
import torch.nn.functional as F
import math, time, gc, os, sys
from dataclasses import dataclass, asdict
MAX_SEQ_LEN = {seq_len}
HEAD_DIM = 64
ASPECT_RATIO = 64
WINDOW_PATTERN = "{window}"
vocab_size = {vocab_size}
DEPTH = {depth}
""".format(seq_len=cd["sequence_len"], window=cd["window_pattern"],
           vocab_size=cd["vocab_size"], depth=cd["n_layer"])

    namespace = {}
    exec(preamble + model_code, namespace)

    GPTConfig = namespace["GPTConfig"]
    GPT = namespace["GPT"]

    # Use checkpoint config directly (build_model_config may compute different values)
    config = GPTConfig(**cd)
    model = GPT(config).to(device).to(torch.bfloat16)
    model.load_state_dict(ckpt["model"])
    model.eval()

    # Cleanup fake module
    del sys.modules["prepare"]

    return model, ckpt.get("val_bpb", 0)


@torch.no_grad()
def generate(model, tokenizer, prompt, max_tokens=300, temperature=0.7, top_k=40):
    tokens = tokenizer.encode_ordinary(prompt)
    x = torch.tensor([tokens], dtype=torch.long, device=device)
    for _ in range(max_tokens):
        x_cond = x[:, -2048:]
        logits = model(x_cond)[:, -1, :] / temperature
        if top_k > 0:
            v, _ = torch.topk(logits, min(top_k, logits.size(-1)))
            logits[logits < v[:, [-1]]] = float('-inf')
        next_token = torch.multinomial(F.softmax(logits, dim=-1), num_samples=1)
        x = torch.cat([x, next_token], dim=1)
    return tokenizer.decode(x[0].tolist())


if __name__ == "__main__":
    prompt = " ".join(sys.argv[1:]) if len(sys.argv) > 1 else "The optimal entry threshold for pairs trading"
    tokenizer = load_tokenizer()
    model, val_bpb = load_model()
    print(f"Model loaded (val_bpb={val_bpb:.4f}, device={device})")
    print(f"\nPrompt: {prompt}")
    print("=" * 60)
    print(generate(model, tokenizer, prompt))
    print("=" * 60)
