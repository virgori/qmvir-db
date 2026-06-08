"""QM Pipelines — Search Indexer.

Consumes CDC/outbox events and updates the search index:
  - Tokenize document fields
  - Update inverted index
  - Update BM25 scorer
  - Handle insert/update/delete
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from search_platform.tokenizer.analyzer import TextAnalyzer
from search_platform.lexical_search.bm25 import LexicalSearchEngine
from core_db.wal_cdc.wal import CDCEvent


@dataclass
class IndexableDocument:
    """A document ready for search indexing."""

    doc_id: str
    collection: str
    fields: dict[str, str]  # field_name -> text content
    metadata: dict[str, Any] | None = None


class SearchIndexer:
    """Indexes documents from CDC events into the search engine."""

    def __init__(
        self,
        search_engine: LexicalSearchEngine,
        analyzer: TextAnalyzer | None = None,
    ) -> None:
        self._engine = search_engine
        self._analyzer = analyzer or TextAnalyzer()
        self._indexed_count = 0
        self._error_count = 0

    def handle_cdc_event(self, event: CDCEvent) -> bool:
        """Process a CDC event for search indexing."""
        try:
            if event.operation == "delete":
                # Remove from search index
                collection = event.table
                scorer = self._engine._collections.get(collection)
                if scorer:
                    scorer.remove_document(event.pk)
                self._indexed_count += 1
                return True

            if event.operation in ("insert", "update"):
                if event.new_data:
                    doc = self._extract_document(event)
                    if doc:
                        self._index_document(doc)
                        self._indexed_count += 1
                return True

            return True
        except Exception:
            self._error_count += 1
            return False

    def index_document(self, doc: IndexableDocument) -> None:
        """Directly index a document."""
        self._index_document(doc)
        self._indexed_count += 1

    def _index_document(self, doc: IndexableDocument) -> None:
        """Tokenize and index a document."""
        tokenized_fields: dict[str, list[str]] = {}
        for field_name, text in doc.fields.items():
            tokens = self._analyzer.analyze(text)
            tokenized_fields[field_name] = tokens

        self._engine.index_document(doc.collection, doc.doc_id, tokenized_fields)

    def _extract_document(self, event: CDCEvent) -> IndexableDocument | None:
        """Extract indexable document from CDC event data."""
        if not event.new_data:
            return None

        # Extract text fields (heuristic: string values)
        fields: dict[str, str] = {}
        for key, value in event.new_data.items():
            if isinstance(value, str) and len(value) > 0:
                fields[key] = value

        if not fields:
            return None

        return IndexableDocument(
            doc_id=event.pk,
            collection=event.table,
            fields=fields,
        )

    @property
    def stats(self) -> dict[str, int]:
        return {"indexed": self._indexed_count, "errors": self._error_count}
