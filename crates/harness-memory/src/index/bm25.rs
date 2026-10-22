//! BM25 search over notes with an incremental, persisted index.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::drive::{DriveError, Note, NotePath, io_err, list_notes, read_note_at};

/// BM25 term-frequency saturation.
pub const BM25_K1: f64 = 1.2;
/// BM25 length normalisation.
pub const BM25_B: f64 = 0.75;
/// File under `.harness/` holding the cached index.
pub const INDEX_FILE: &str = "search-index.json";
/// Most matching bullets returned per hit.
pub const MAX_SNIPPETS: usize = 3;

const FORMAT_VERSION: u32 = 1;

/// Splits text into lower-case alphanumeric tokens (`_` kept inside words).
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// One indexed note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocEntry {
    /// sha256 of the note text; unchanged notes are not re-indexed.
    pub hash: String,
    /// Number of tokens.
    pub len: u32,
    /// Term frequencies.
    pub tf: BTreeMap<String, u32>,
    /// The note's title and bullet texts, for snippets.
    pub lines: Vec<String>,
}

impl DocEntry {
    fn build(text: &str) -> Self {
        let note = Note::parse(text);
        let mut lines: Vec<String> = Vec::new();
        if let Some(t) = note.title() {
            lines.push(format!("# {t}"));
        }
        lines.extend(note.bullets().map(|b| b.text.clone()));
        let mut tf = BTreeMap::new();
        let mut len = 0u32;
        for l in &lines {
            for t in tokenize(l) {
                *tf.entry(t).or_insert(0) += 1;
                len += 1;
            }
        }
        Self {
            hash: hash(text),
            len,
            tf,
            lines,
        }
    }
}

fn hash(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// What an [`SearchIndex::update`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexStats {
    /// Notes indexed for the first time.
    pub added: usize,
    /// Notes re-indexed because their content changed.
    pub updated: usize,
    /// Notes dropped because they no longer exist.
    pub removed: usize,
    /// Notes left as they were.
    pub unchanged: usize,
}

/// A search result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHit {
    /// The note.
    pub path: String,
    /// BM25 score (higher is better).
    pub score: f64,
    /// The note's title, if any.
    pub title: Option<String>,
    /// Up to [`MAX_SNIPPETS`] bullets containing query terms, best first.
    pub snippets: Vec<String>,
}

/// BM25 index over the notes of one checkout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchIndex {
    version: u32,
    docs: BTreeMap<String, DocEntry>,
}

impl Default for SearchIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self {
            version: FORMAT_VERSION,
            docs: BTreeMap::new(),
        }
    }

    /// Loads a cached index, or an empty one when the file is missing,
    /// unreadable or from another format version (it is only a cache).
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Self>(&b).ok())
            .filter(|i| i.version == FORMAT_VERSION)
            .unwrap_or_default()
    }

    /// Writes the index atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> Result<(), DriveError> {
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        let bytes = serde_json::to_vec(self).map_err(|e| DriveError::Io {
            context: "encoding the search index".into(),
            source: io::Error::other(e),
        })?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(io_err("creating the index directory"))?;
        }
        fs::write(&tmp, bytes).map_err(io_err("writing the search index"))?;
        fs::rename(&tmp, path).map_err(io_err("replacing the search index"))
    }

    /// Brings the index in line with the notes under `checkout`. Only notes
    /// whose content hash changed are re-tokenised.
    pub fn update(&mut self, checkout: &Path) -> Result<IndexStats, DriveError> {
        let mut stats = IndexStats::default();
        let mut seen = BTreeSet::new();
        for np in list_notes(checkout)? {
            let Some(text) = read_note_at(checkout, &np)? else {
                continue;
            };
            let key = np.as_str().to_owned();
            let h = hash(&text);
            match self.docs.get(&key) {
                Some(d) if d.hash == h => stats.unchanged += 1,
                Some(_) => {
                    self.docs.insert(key.clone(), DocEntry::build(&text));
                    stats.updated += 1;
                }
                None => {
                    self.docs.insert(key.clone(), DocEntry::build(&text));
                    stats.added += 1;
                }
            }
            seen.insert(key);
        }
        let before = self.docs.len();
        self.docs.retain(|k, _| seen.contains(k));
        stats.removed = before - self.docs.len();
        Ok(stats)
    }

    /// Re-indexes one note from `text` (or drops it when `None`).
    pub fn upsert(&mut self, path: &NotePath, text: Option<&str>) {
        match text {
            Some(t) => {
                self.docs
                    .insert(path.as_str().to_owned(), DocEntry::build(t));
            }
            None => {
                self.docs.remove(path.as_str());
            }
        }
    }

    /// Number of indexed notes.
    pub fn len(&self) -> usize {
        self.docs.len()
    }

    /// True when nothing is indexed.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// The entry for a note.
    pub fn doc(&self, path: &str) -> Option<&DocEntry> {
        self.docs.get(path)
    }

    /// BM25 search. Returns at most `limit` hits with a positive score, best
    /// first (ties broken by path).
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let terms: BTreeSet<String> = tokenize(query).into_iter().collect();
        if terms.is_empty() || self.docs.is_empty() {
            return Vec::new();
        }
        let n = self.docs.len() as f64;
        let avgdl = (self.docs.values().map(|d| f64::from(d.len)).sum::<f64>() / n).max(1.0);
        let idf: BTreeMap<&str, f64> = terms
            .iter()
            .map(|t| {
                let df = self.docs.values().filter(|d| d.tf.contains_key(t)).count() as f64;
                (t.as_str(), ((n - df + 0.5) / (df + 0.5) + 1.0).ln())
            })
            .collect();
        let mut hits: Vec<SearchHit> = self
            .docs
            .iter()
            .filter_map(|(path, d)| {
                let dl = f64::from(d.len);
                let score: f64 = terms
                    .iter()
                    .filter_map(|t| {
                        let f = f64::from(*d.tf.get(t)?);
                        let norm = BM25_K1 * (1.0 - BM25_B + BM25_B * dl / avgdl);
                        Some(idf[t.as_str()] * f * (BM25_K1 + 1.0) / (f + norm))
                    })
                    .sum();
                (score > 0.0).then(|| SearchHit {
                    path: path.clone(),
                    score,
                    title: d
                        .lines
                        .first()
                        .and_then(|l| l.strip_prefix("# "))
                        .map(str::to_owned),
                    snippets: snippets(d, &terms),
                })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.path.cmp(&b.path))
        });
        hits.truncate(limit);
        hits
    }
}

fn snippets(d: &DocEntry, terms: &BTreeSet<String>) -> Vec<String> {
    let mut scored: Vec<(usize, usize, &String)> = d
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.starts_with("# "))
        .map(|(i, l)| {
            let toks: BTreeSet<String> = tokenize(l).into_iter().collect();
            (toks.intersection(terms).count(), i, l)
        })
        .filter(|(c, _, _)| *c > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(MAX_SNIPPETS)
        .map(|(_, _, l)| l.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    #[test]
    fn tokenizer_lowercases_and_splits() {
        assert_eq!(
            tokenize("Run `just test-int`; cargo_test!"),
            vec!["run", "just", "test", "int", "cargo_test"]
        );
    }

    #[test]
    fn bm25_ranks_rarer_and_denser_matches_higher() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        write(r, "MEMORY.md", "# Memory\n- prefer short answers\n");
        write(
            r,
            "tools/docker.md",
            "# Docker\n- docker compose needs the --wait flag for healthchecks\n",
        );
        write(
            r,
            "projects/app-1/testing.md",
            "# Testing\n- integration tests need docker running\n- use just test-int\n",
        );
        let mut idx = SearchIndex::new();
        idx.update(r).unwrap();
        let hits = idx.search("docker compose", 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, "tools/docker.md");
        assert_eq!(hits[0].title.as_deref(), Some("Docker"));
        assert_eq!(
            hits[0].snippets,
            vec!["docker compose needs the --wait flag for healthchecks"]
        );
        assert!(hits[0].score > hits[1].score);
        assert!(idx.search("kubernetes", 10).is_empty());
        assert!(idx.search("   ", 10).is_empty());
        assert_eq!(idx.search("docker", 1).len(), 1);
    }

    #[test]
    fn update_is_incremental_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        write(r, "MEMORY.md", "# Memory\n- a\n");
        write(r, "tools/cargo.md", "# Cargo\n- use nextest\n");
        let mut idx = SearchIndex::new();
        assert_eq!(
            idx.update(r).unwrap(),
            IndexStats {
                added: 2,
                ..Default::default()
            }
        );
        write(
            r,
            "tools/cargo.md",
            "# Cargo\n- use nextest\n- clippy pedantic\n",
        );
        write(r, "tools/git.md", "# Git\n- rebase\n");
        fs::remove_file(r.join("MEMORY.md")).unwrap();
        assert_eq!(
            idx.update(r).unwrap(),
            IndexStats {
                added: 1,
                updated: 1,
                removed: 1,
                unchanged: 0
            }
        );
        let file = r.join(".harness").join(INDEX_FILE);
        idx.save(&file).unwrap();
        let mut loaded = SearchIndex::load(&file);
        assert_eq!(loaded, idx);
        assert_eq!(
            loaded.update(r).unwrap(),
            IndexStats {
                unchanged: 2,
                ..Default::default()
            }
        );
        assert_eq!(loaded.search("pedantic", 5)[0].path, "tools/cargo.md");
        // A corrupt cache is ignored.
        fs::write(&file, "not json").unwrap();
        assert!(SearchIndex::load(&file).is_empty());
    }
}
