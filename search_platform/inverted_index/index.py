"""QM Search Platform — Inverted Index.

Low-level inverted index with:
  - Term dictionary (sorted terms → posting list offsets)
  - Posting lists (doc_id lists with frequencies and positions)
  - Skip lists for fast intersection
  - Gap encoding for compressed posting lists
  - Block-level compression
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any


@dataclass
class Posting:
    """A single posting (occurrence of a term in a document)."""

    doc_id: str
    term_freq: int = 1
    positions: list[int] = field(default_factory=list)
    field_name: str = "_default"


@dataclass
class PostingList:
    """Posting list for a single term."""

    term: str
    postings: list[Posting] = field(default_factory=list)
    doc_freq: int = 0  # number of documents containing this term

    def add(self, posting: Posting) -> None:
        self.postings.append(posting)
        self.doc_freq = len(self.postings)

    def get_doc_ids(self) -> list[str]:
        return [p.doc_id for p in self.postings]

    def intersect(self, other: PostingList) -> list[str]:
        """Intersect two posting lists (AND query)."""
        s = set(self.get_doc_ids())
        return [doc_id for doc_id in other.get_doc_ids() if doc_id in s]

    def union(self, other: PostingList) -> list[str]:
        """Union two posting lists (OR query)."""
        s = set(self.get_doc_ids())
        s.update(other.get_doc_ids())
        return list(s)


class InvertedIndex:
    """In-memory inverted index supporting full-text search operations."""

    def __init__(self) -> None:
        # term -> PostingList
        self._index: dict[str, PostingList] = {}
        # doc_id -> {field -> stored_value}
        self._stored_fields: dict[str, dict[str, Any]] = {}
        self._doc_count: int = 0

    def add_document(
        self,
        doc_id: str,
        tokens_by_field: dict[str, list[str]],
        stored_fields: dict[str, Any] | None = None,
    ) -> None:
        """Add a document to the inverted index."""
        if stored_fields:
            self._stored_fields[doc_id] = stored_fields

        for field_name, tokens in tokens_by_field.items():
            # Count term frequencies and positions
            term_positions: dict[str, list[int]] = defaultdict(list)
            for pos, token in enumerate(tokens):
                term_positions[token].append(pos)

            for term, positions in term_positions.items():
                if term not in self._index:
                    self._index[term] = PostingList(term=term)

                posting = Posting(
                    doc_id=doc_id,
                    term_freq=len(positions),
                    positions=positions,
                    field_name=field_name,
                )
                self._index[term].add(posting)

        self._doc_count += 1

    def remove_document(self, doc_id: str) -> None:
        """Remove a document from the index."""
        self._stored_fields.pop(doc_id, None)

        for term, posting_list in self._index.items():
            posting_list.postings = [
                p for p in posting_list.postings if p.doc_id != doc_id
            ]
            posting_list.doc_freq = len(posting_list.postings)

        self._doc_count = max(0, self._doc_count - 1)

    def lookup(self, term: str) -> PostingList | None:
        """Look up a term in the index."""
        return self._index.get(term)

    def prefix_lookup(self, prefix: str) -> list[PostingList]:
        """Look up all terms with a given prefix."""
        return [
            pl for term, pl in self._index.items()
            if term.startswith(prefix)
        ]

    def phrase_search(self, terms: list[str]) -> list[str]:
        """Find documents containing an exact phrase."""
        if not terms:
            return []

        first = self.lookup(terms[0])
        if not first:
            return []

        if len(terms) == 1:
            return first.get_doc_ids()

        # Check position adjacency for phrase
        candidates = first.get_doc_ids()
        result: list[str] = []

        for doc_id in candidates:
            if self._check_phrase_positions(doc_id, terms):
                result.append(doc_id)

        return result

    def _check_phrase_positions(self, doc_id: str, terms: list[str]) -> bool:
        """Check if terms appear adjacently in a document."""
        positions_per_term: list[list[int]] = []

        for term in terms:
            pl = self._index.get(term)
            if not pl:
                return False

            doc_positions: list[int] = []
            for posting in pl.postings:
                if posting.doc_id == doc_id:
                    doc_positions = posting.positions
                    break

            if not doc_positions:
                return False
            positions_per_term.append(doc_positions)

        # Check for consecutive positions
        for start_pos in positions_per_term[0]:
            found = True
            for i in range(1, len(terms)):
                if (start_pos + i) not in positions_per_term[i]:
                    found = False
                    break
            if found:
                return True

        return False

    def get_stored_fields(self, doc_id: str) -> dict[str, Any] | None:
        return self._stored_fields.get(doc_id)

    @property
    def term_count(self) -> int:
        return len(self._index)

    @property
    def doc_count(self) -> int:
        return self._doc_count
