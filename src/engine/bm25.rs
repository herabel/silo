//! Deterministic in-memory Okapi BM25 chunk ranker for RAG.
//!
//! Pure Rust, zero external crate dependencies.

#[derive(Debug, Clone, PartialEq)]
pub struct ScoredChunk {
    pub score: f32,
    pub chunk: String,
    pub index: usize,
}

/// Split text into chunks of ~target_chunk_len characters.
///
/// - Splits on double-newline `\n\n` into paragraphs.
/// - Groups consecutive small paragraphs together until reaching ~target_chunk_len.
/// - If a paragraph is larger than target_chunk_len * 2, splits it by single newlines.
/// - Trims whitespace and discards empty chunks.
pub fn split_into_chunks(text: &str, target_chunk_len: usize) -> Vec<String> {
    let mut paragraphs: Vec<String> = Vec::new();

    for raw_para in text.split("\n\n") {
        let para = raw_para.trim();
        if para.is_empty() {
            continue;
        }
        if para.len() > target_chunk_len {
            // Split large paragraph by single newlines
            for line in para.split('\n') {
                let line = line.trim();
                if !line.is_empty() {
                    paragraphs.push(line.to_string());
                }
            }
        } else {
            paragraphs.push(para.to_string());
        }
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    for para in paragraphs {
        if current.is_empty() {
            current = para;
            continue;
        }
        if current.len() + 2 + para.len() <= target_chunk_len {
            current.push_str("\n\n");
            current.push_str(&para);
        } else {
            chunks.push(current);
            current = para;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
        .into_iter()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect()
}

/// Tokenize text: lowercase, split on any character that is not alphanumeric, '_' or '-'.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Rank chunks of `text` against `query` using Okapi BM25.
///
/// Returns up to `top_k` chunks with score > 0.0, sorted by score descending
/// (tie-breaker: original index ascending).
pub fn rank_chunks_bm25(text: &str, query: &str, top_k: usize) -> Vec<ScoredChunk> {
    let chunks = split_into_chunks(text, 700);
    if chunks.is_empty() {
        return Vec::new();
    }

    let k1: f32 = 1.2;
    let b: f32 = 0.75;

    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        return Vec::new();
    }

    let chunk_tokens: Vec<Vec<String>> = chunks.iter().map(|c| tokenize(c)).collect();

    let n = chunks.len() as f32;
    let avgdl = chunk_tokens.iter().map(|t| t.len() as f32).sum::<f32>() / n;

    // Count how many chunks contain each query token
    let mut token_doc_freq: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for token in &query_tokens {
        let mut count = 0usize;
        for tokens in &chunk_tokens {
            if tokens.iter().any(|t| t == token) {
                count += 1;
            }
        }
        token_doc_freq.insert(token.clone(), count);
    }

    // Compute IDF for each query token
    let mut idf: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
    for token in &query_tokens {
        let n_docs = *token_doc_freq.get(token).unwrap_or(&0) as f32;
        let idf_val = ((n - n_docs + 0.5) / (n_docs + 0.5) + 1.0).ln();
        idf.insert(token.clone(), idf_val);
    }

    let mut scored: Vec<ScoredChunk> = Vec::new();
    for (idx, tokens) in chunk_tokens.iter().enumerate() {
        let dl = tokens.len() as f32;
        let mut score = 0.0f32;
        for token in &query_tokens {
            let f = tokens.iter().filter(|t| *t == token).count() as f32;
            if f == 0.0 {
                continue;
            }
            let idf_val = *idf.get(token).unwrap_or(&0.0);
            let denom = f + k1 * (1.0 - b + b * (dl / avgdl));
            score += idf_val * (f * (k1 + 1.0)) / denom;
        }
        if score > 0.0 {
            scored.push(ScoredChunk {
                score,
                chunk: chunks[idx].clone(),
                index: idx,
            });
        }
    }

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.index.cmp(&b.index))
    });

    scored.truncate(top_k);
    scored
}

/// Format ranked chunks with headers.
pub fn format_bm25_results(chunks: &[ScoredChunk]) -> String {
    let mut out = String::new();
    for c in chunks {
        out.push_str(&format!("[Chunk {} | BM25: {:.2}]\n{}\n\n", c.index + 1, c.score, c.chunk));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bm25_ranking() {
        let text = "Kyber is a post-quantum key encapsulation mechanism based on lattice cryptography. \
                    It is designed to resist attacks from quantum computers and is part of the NIST standardization process. \
                    Kyber relies on the hardness of the Learning With Errors problem.

                    The weather in Vologda is cold and snowy in winter, with temperatures often dropping below -20 degrees Celsius. \
                    Summers are short and mild, with average temperatures around 18 degrees. \
                    The region experiences heavy snowfall and strong winds during the winter months.

                    Rust async runtime uses a work-stealing scheduler to multiplex tasks across a thread pool. \
                    Futures are polled cooperatively and yield control at await points. \
                    The runtime is designed for high concurrency with minimal overhead.";

        let kyber_results = rank_chunks_bm25(text, "Kyber", 2);
        assert!(!kyber_results.is_empty(), "expected Kyber results");
        assert!(
            kyber_results[0].chunk.contains("Kyber"),
            "expected Kyber paragraph #1, got: {}",
            kyber_results[0].chunk
        );

        let weather_results = rank_chunks_bm25(text, "Vologda weather", 2);
        assert!(!weather_results.is_empty(), "expected weather results");
        assert!(
            weather_results[0].chunk.contains("Vologda"),
            "expected weather paragraph #1, got: {}",
            weather_results[0].chunk
        );

        let rust_results = rank_chunks_bm25(text, "Rust async runtime", 2);
        assert!(!rust_results.is_empty(), "expected Rust results");
        assert!(
            rust_results[0].chunk.contains("Rust"),
            "expected Rust paragraph #1, got: {}",
            rust_results[0].chunk
        );
    }

    #[test]
    fn test_split_into_chunks_groups_small_paragraphs() {
        let text = "short para one\n\nshort para two\n\nshort para three";
        let chunks = split_into_chunks(text, 700);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("short para one"));
        assert!(chunks[0].contains("short para three"));
    }

    #[test]
    fn test_split_into_chunks_splits_large_paragraph() {
        let big = "line one\nline two\nline three\nline four\nline five";
        let text = big.repeat(20); // > 1400 chars
        let chunks = split_into_chunks(&text, 700);
        assert!(chunks.len() > 1);
    }

    #[test]
    fn test_format_bm25_results() {
        let chunks = vec![ScoredChunk {
            score: 1.234,
            chunk: "hello world".to_string(),
            index: 0,
        }];
        let out = format_bm25_results(&chunks);
        assert!(out.contains("[Chunk 1 | BM25: 1.23]"));
        assert!(out.contains("hello world"));
    }
}
