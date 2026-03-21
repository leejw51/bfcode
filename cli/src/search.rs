use std::collections::HashMap;

/// TF-IDF search index for memory vector search.
///
/// Provides semantic-like search over memory markdown files
/// stored in `.bfcode/memory/` using term frequency-inverse
/// document frequency scoring.
pub struct TfidfIndex {
    /// Document frequency: how many docs contain each term
    doc_freq: HashMap<String, usize>,
    /// Term frequency per document: doc_index -> (term -> count)
    tf: Vec<HashMap<String, usize>>,
    /// Document names
    doc_names: Vec<String>,
    /// Document contents (for returning snippets)
    doc_contents: Vec<String>,
    /// Total number of documents
    num_docs: usize,
}

/// A single search result with relevance score and text snippet.
pub struct SearchResult {
    pub name: String,
    pub score: f64,
    pub snippet: String,
}

/// Common English stop words to filter out during tokenization.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "but", "by", "do", "for", "from", "had",
    "has", "have", "he", "her", "his", "how", "if", "in", "into", "is", "it", "its", "just",
    "me", "my", "no", "not", "of", "on", "or", "our", "out", "so", "than", "that", "the",
    "their", "them", "then", "there", "these", "they", "this", "to", "up", "us", "was", "we",
    "were", "what", "when", "which", "who", "will", "with", "would", "you", "your",
];

impl TfidfIndex {
    /// Build a TF-IDF index from (name, content) pairs.
    ///
    /// Tokenizes each document, computes term frequencies per document,
    /// and computes document frequencies across all documents.
    pub fn build(documents: Vec<(String, String)>) -> Self {
        let num_docs = documents.len();
        let mut doc_names = Vec::with_capacity(num_docs);
        let mut doc_contents = Vec::with_capacity(num_docs);
        let mut tf = Vec::with_capacity(num_docs);
        let mut doc_freq: HashMap<String, usize> = HashMap::new();

        for (name, content) in &documents {
            doc_names.push(name.clone());
            doc_contents.push(content.clone());

            let tokens = tokenize(content);
            let mut term_counts: HashMap<String, usize> = HashMap::new();
            for token in &tokens {
                *term_counts.entry(token.clone()).or_insert(0) += 1;
            }

            // Each unique term in this document increments its document frequency
            for term in term_counts.keys() {
                *doc_freq.entry(term.clone()).or_insert(0) += 1;
            }

            tf.push(term_counts);
        }

        Self {
            doc_freq,
            tf,
            doc_names,
            doc_contents,
            num_docs,
        }
    }

    /// Search the index, returning top-k results sorted by relevance (highest first).
    ///
    /// Scoring uses standard TF-IDF:
    /// - TF = term_count / total_terms_in_doc
    /// - IDF = ln(num_docs / (1 + doc_freq))
    /// - Score = sum of (TF * IDF) for each query term
    ///
    /// Results with a score of zero are excluded.
    pub fn search(&self, query: &str, top_k: usize) -> Vec<SearchResult> {
        if self.num_docs == 0 {
            return Vec::new();
        }

        let query_tokens = tokenize(query);
        if query_tokens.is_empty() {
            return Vec::new();
        }

        let mut scores: Vec<(usize, f64)> = Vec::with_capacity(self.num_docs);

        for (doc_idx, term_counts) in self.tf.iter().enumerate() {
            let total_terms: usize = term_counts.values().sum();
            if total_terms == 0 {
                continue;
            }

            let mut score = 0.0_f64;

            for token in &query_tokens {
                let term_count = match term_counts.get(token) {
                    Some(&c) => c,
                    None => continue,
                };

                let tf = term_count as f64 / total_terms as f64;

                let df = self.doc_freq.get(token).copied().unwrap_or(0);
                let idf = (self.num_docs as f64 / (1 + df) as f64).ln();

                score += tf * idf;
            }

            if score > 0.0 {
                scores.push((doc_idx, score));
            }
        }

        // Sort descending by score
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(top_k);

        scores
            .into_iter()
            .map(|(idx, score)| {
                let snippet = make_snippet(&self.doc_contents[idx]);
                SearchResult {
                    name: self.doc_names[idx].clone(),
                    score,
                    snippet,
                }
            })
            .collect()
    }
}

/// Tokenize text into lowercase terms, splitting on non-alphanumeric characters.
///
/// Filters out:
/// - Tokens shorter than 2 characters
/// - Common English stop words
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= 2)
        .filter(|s| !STOP_WORDS.contains(s))
        .map(|s| s.to_string())
        .collect()
}

/// Extract a short snippet from document content (first ~200 chars, trimmed to word boundary).
fn make_snippet(content: &str) -> String {
    let max_len = 200;
    if content.len() <= max_len {
        return content.trim().to_string();
    }

    // Find a word boundary near the limit
    let truncated = &content[..max_len];
    match truncated.rfind(|c: char| c.is_whitespace()) {
        Some(pos) => format!("{}...", &content[..pos]),
        None => format!("{}...", truncated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_basic() {
        let tokens = tokenize("Hello, World! This is a test.");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"test".to_string()));
        // Stop words should be filtered
        assert!(!tokens.contains(&"this".to_string()));
        assert!(!tokens.contains(&"is".to_string()));
    }

    #[test]
    fn test_tokenize_filters_short() {
        let tokens = tokenize("I am a b c good");
        // "I", "a", "b", "c" are all < 2 chars
        assert!(!tokens.contains(&"i".to_string()));
        assert!(!tokens.contains(&"b".to_string()));
        assert!(!tokens.contains(&"c".to_string()));
        assert!(tokens.contains(&"good".to_string()));
    }

    #[test]
    fn test_empty_index() {
        let index = TfidfIndex::build(vec![]);
        let results = index.search("anything", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_relevance() {
        let docs = vec![
            (
                "rust.md".to_string(),
                "Rust is a systems programming language focused on safety".to_string(),
            ),
            (
                "python.md".to_string(),
                "Python is a dynamic programming language for scripting".to_string(),
            ),
            (
                "cooking.md".to_string(),
                "Recipe for chocolate cake with vanilla frosting".to_string(),
            ),
        ];

        let index = TfidfIndex::build(docs);
        let results = index.search("rust programming", 3);

        assert!(!results.is_empty());
        // "rust.md" should be the top result
        assert_eq!(results[0].name, "rust.md");
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn test_search_no_match() {
        let docs = vec![(
            "doc.md".to_string(),
            "completely unrelated content here".to_string(),
        )];

        let index = TfidfIndex::build(docs);
        let results = index.search("quantum physics", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_top_k_limit() {
        let docs: Vec<(String, String)> = (0..10)
            .map(|i| (format!("doc{}.md", i), format!("search term number {}", i)))
            .collect();

        let index = TfidfIndex::build(docs);
        let results = index.search("search term", 3);
        assert!(results.len() <= 3);
    }

    #[test]
    fn test_snippet_truncation() {
        let long_text = "word ".repeat(100);
        let snippet = make_snippet(&long_text);
        assert!(snippet.len() < long_text.len());
        assert!(snippet.ends_with("..."));
    }

    #[test]
    fn test_snippet_short_content() {
        let short = "Short content.";
        let snippet = make_snippet(short);
        assert_eq!(snippet, "Short content.");
    }
}
