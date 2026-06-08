"""QM Search Platform — Synonym expansion engine."""

from __future__ import annotations

from collections import defaultdict


class SynonymEngine:
    """Expands query terms with synonyms for broader recall."""

    def __init__(self) -> None:
        # term -> set of synonyms (bidirectional)
        self._synonyms: dict[str, set[str]] = defaultdict(set)

    def add_synonym_group(self, terms: list[str]) -> None:
        """Add a group of synonyms (all are equivalent)."""
        for term in terms:
            for other in terms:
                if term != other:
                    self._synonyms[term.lower()].add(other.lower())

    def add_one_way(self, source: str, targets: list[str]) -> None:
        """Add one-way synonym expansion (source → targets)."""
        for target in targets:
            self._synonyms[source.lower()].add(target.lower())

    def expand(self, term: str) -> list[str]:
        """Expand a term to include its synonyms."""
        lower = term.lower()
        syns = self._synonyms.get(lower, set())
        return [lower] + sorted(syns)

    def expand_query(self, tokens: list[str]) -> list[str]:
        """Expand all tokens in a query."""
        expanded: list[str] = []
        for token in tokens:
            expanded.extend(self.expand(token))
        return expanded

    def load_from_dict(self, synonym_map: dict[str, list[str]]) -> None:
        """Batch load synonyms from a mapping."""
        for key, values in synonym_map.items():
            self.add_synonym_group([key] + values)
