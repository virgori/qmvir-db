"""QM Search Platform — Text Tokenizer / Analyzer.

Pipeline:
  1. Character filter (strip HTML, normalize unicode)
  2. Tokenizer (whitespace, ngram, edge-ngram)
  3. Token filter (lowercase, stemming, stopword, synonym)
"""

from __future__ import annotations

import re
import unicodedata
from dataclasses import dataclass, field
from typing import Callable


@dataclass
class Token:
    """A single token from analysis."""

    text: str
    position: int
    start_offset: int = 0
    end_offset: int = 0


class CharFilter:
    """Character-level pre-processing."""

    @staticmethod
    def strip_html(text: str) -> str:
        return re.sub(r"<[^>]+>", " ", text)

    @staticmethod
    def normalize_unicode(text: str) -> str:
        return unicodedata.normalize("NFKC", text)

    @staticmethod
    def strip_punctuation(text: str) -> str:
        return re.sub(r"[^\w\s]", " ", text, flags=re.UNICODE)

    @staticmethod
    def collapse_whitespace(text: str) -> str:
        return re.sub(r"\s+", " ", text).strip()


class TokenFilter:
    """Token-level post-processing."""

    @staticmethod
    def lowercase(tokens: list[Token]) -> list[Token]:
        for t in tokens:
            t.text = t.text.lower()
        return tokens

    @staticmethod
    def remove_stopwords(tokens: list[Token], stopwords: set[str]) -> list[Token]:
        return [t for t in tokens if t.text not in stopwords]

    @staticmethod
    def min_length(tokens: list[Token], min_len: int = 2) -> list[Token]:
        return [t for t in tokens if len(t.text) >= min_len]

    @staticmethod
    def edge_ngrams(tokens: list[Token], min_n: int = 2, max_n: int = 10) -> list[Token]:
        """Generate edge n-grams for autocomplete."""
        result: list[Token] = []
        for t in tokens:
            for n in range(min_n, min(max_n + 1, len(t.text) + 1)):
                result.append(Token(
                    text=t.text[:n],
                    position=t.position,
                    start_offset=t.start_offset,
                    end_offset=t.start_offset + n,
                ))
        return result


# Default English stopwords
DEFAULT_STOPWORDS: set[str] = {
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for",
    "if", "in", "into", "is", "it", "no", "not", "of", "on", "or",
    "such", "that", "the", "their", "then", "there", "these", "they",
    "this", "to", "was", "will", "with",
}


@dataclass
class AnalyzerConfig:
    """Tokenizer/analyzer configuration."""

    strip_html: bool = True
    normalize_unicode: bool = True
    strip_punctuation: bool = True
    lowercase: bool = True
    remove_stopwords: bool = True
    stopwords: set[str] = field(default_factory=lambda: DEFAULT_STOPWORDS)
    min_token_length: int = 1
    edge_ngrams: bool = False
    edge_ngram_min: int = 2
    edge_ngram_max: int = 10


class TextAnalyzer:
    """Full text analysis pipeline: char filters → tokenize → token filters."""

    def __init__(self, config: AnalyzerConfig | None = None) -> None:
        self.config = config or AnalyzerConfig()

    def analyze(self, text: str) -> list[str]:
        """Run full analysis pipeline and return token strings."""
        tokens = self.tokenize(text)
        return [t.text for t in tokens]

    def tokenize(self, text: str) -> list[Token]:
        """Full tokenization pipeline."""
        # Character filters
        if self.config.strip_html:
            text = CharFilter.strip_html(text)
        if self.config.normalize_unicode:
            text = CharFilter.normalize_unicode(text)
        if self.config.strip_punctuation:
            text = CharFilter.strip_punctuation(text)
        text = CharFilter.collapse_whitespace(text)

        # Whitespace tokenization
        words = text.split()
        tokens = [
            Token(text=w, position=i, start_offset=0, end_offset=len(w))
            for i, w in enumerate(words)
        ]

        # Token filters
        if self.config.lowercase:
            tokens = TokenFilter.lowercase(tokens)
        if self.config.remove_stopwords:
            tokens = TokenFilter.remove_stopwords(tokens, self.config.stopwords)
        if self.config.min_token_length > 1:
            tokens = TokenFilter.min_length(tokens, self.config.min_token_length)
        if self.config.edge_ngrams:
            tokens = TokenFilter.edge_ngrams(
                tokens, self.config.edge_ngram_min, self.config.edge_ngram_max
            )

        return tokens
