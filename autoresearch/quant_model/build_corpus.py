"""
Convert our quant corpus text files into parquet shards
compatible with autoresearch's prepare.py.

Each shard is a parquet file with a single 'text' column.
We chunk the corpus into ~50MB shards.

Usage: python build_corpus.py
"""

import os
import sys

import pyarrow as pa
import pyarrow.parquet as pq

CORPUS_DIR = os.path.join(os.path.dirname(__file__), "..", "corpus")
CACHE_DIR = os.path.join(os.path.expanduser("~"), ".cache", "autoresearch-quant")
DATA_DIR = os.path.join(CACHE_DIR, "data")

# Chunk size for splitting documents
MAX_DOC_CHARS = 4000  # split long docs into ~4K char chunks


def read_corpus():
    """Read all text files from corpus dir, yield documents."""
    for fname in sorted(os.listdir(CORPUS_DIR)):
        fpath = os.path.join(CORPUS_DIR, fname)
        if not os.path.isfile(fpath):
            continue
        if not (fname.endswith('.txt') or fname.endswith('.toml') or fname.endswith('.tsv') or fname.endswith('.md')):
            continue

        print(f"  Reading {fname} ({os.path.getsize(fpath):,} bytes)")
        with open(fpath, encoding='utf-8', errors='ignore') as f:
            content = f.read()

        # Split into documents at double-newlines (natural paragraph breaks)
        docs = content.split('\n\n')
        for doc in docs:
            doc = doc.strip()
            if len(doc) < 50:  # skip tiny fragments
                continue
            # Chunk long documents
            if len(doc) > MAX_DOC_CHARS:
                for i in range(0, len(doc), MAX_DOC_CHARS):
                    chunk = doc[i:i + MAX_DOC_CHARS].strip()
                    if len(chunk) >= 50:
                        yield chunk
            else:
                yield doc


def build_shards(max_shard_bytes=50_000_000):
    """Build parquet shards from corpus documents."""
    os.makedirs(DATA_DIR, exist_ok=True)

    shard_idx = 0
    current_docs = []
    current_bytes = 0

    for doc in read_corpus():
        current_docs.append(doc)
        current_bytes += len(doc.encode('utf-8'))

        if current_bytes >= max_shard_bytes:
            write_shard(shard_idx, current_docs)
            shard_idx += 1
            current_docs = []
            current_bytes = 0

    # Write remaining docs
    if current_docs:
        write_shard(shard_idx, current_docs)
        shard_idx += 1

    # The last shard is the validation shard (autoresearch convention)
    # Duplicate the last shard as the val shard if we only have a few
    print(f"\nTotal: {shard_idx} shards written to {DATA_DIR}")
    return shard_idx


def write_shard(idx, docs):
    """Write a list of documents as a parquet file."""
    filename = f"shard_{idx:05d}.parquet"
    filepath = os.path.join(DATA_DIR, filename)
    table = pa.table({"text": docs})
    pq.write_table(table, filepath)
    total_chars = sum(len(d) for d in docs)
    print(f"  Wrote {filename}: {len(docs):,} docs, {total_chars:,} chars")


if __name__ == "__main__":
    print("=" * 60)
    print("Building quant corpus shards for autoresearch training")
    print("=" * 60)
    print(f"\nCorpus dir: {CORPUS_DIR}")
    print(f"Output dir: {DATA_DIR}")
    print()

    n_shards = build_shards()

    print()
    print(f"Done! {n_shards} shards ready.")
    print(f"Next: update prepare.py CACHE_DIR to point to {CACHE_DIR}")
    print(f"Then: python prepare.py  (to train tokenizer)")
    print(f"Then: python train.py    (to train model)")
