# Introduction to Search Engines

Search engines are software systems designed to carry out web search (Internet search), which means to search the World Wide Web in a systematic way for particular information specified in a textual web search query.

# Lucene and FST Matching

Apache Lucene is a high-performance, full-featured text search engine library written entirely in Java. It is technology that is suitable for nearly any application that requires full-text search, especially cross-platform.

Lucene uses Finite State Transducers (FST) for extremely fast dictionary matching and term indexing. It allows for O(N) phrase matching speed which is useful for tagging text.

# BM25 Lexical Ranking

BM25 is a family of ranking functions in information retrieval that estimates the relevance of a document to a given search query. It is a bag-of-words retrieval function that ranks a set of documents based on the query terms appearing in each document.

By combining BM25 lexical relevance scoring with FST exact phrase tagging, we create a hybrid search engine that can match semantic entities exactly while ranking documents based on structural query relevance.
