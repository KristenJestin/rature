//! "What changed": the file now against the file as it was last read, block by block, and word
//! by word inside a paragraph or a heading that changed.

use similar::{Algorithm, DiffTag, capture_diff_slices};

use crate::document::{Block, Document, Inline, Marks};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
    Same,
    Inserted,
    Deleted,
}

#[derive(Clone)]
pub struct Word {
    pub text: String,
    pub marks: Marks,
    pub change: Change,
}

pub enum Mark {
    Same,
    Added,
    /// Changed; with its words when it is a paragraph or a heading that was one before, so the
    /// change shows inside it. Otherwise the block as it was stands above it, struck.
    Changed(Option<Vec<Word>>),
}

pub struct Changes {
    /// One per block of the file now.
    pub marks: Vec<Mark>,
    /// Blocks of the file as it was, shown struck before the block of the file now they stood
    /// before (the number of blocks when they were last).
    pub removed: Vec<(usize, usize)>,
}

impl Changes {
    /// The blocks where a run of changes starts, in order: what `]c` and `[c` go through.
    pub fn stops(&self) -> Vec<usize> {
        let mut changed: Vec<usize> = self
            .marks
            .iter()
            .enumerate()
            .filter(|(_, m)| !matches!(m, Mark::Same))
            .map(|(ix, _)| ix)
            .chain(self.removed.iter().map(|(before, _)| *before))
            .collect();
        changed.sort_unstable();
        changed.dedup();
        changed
            .iter()
            .filter(|&&ix| ix == 0 || changed.binary_search(&(ix - 1)).is_err())
            .copied()
            .collect()
    }

    pub fn count(&self) -> usize {
        self.stops().len()
    }
}

pub fn compare(old: &Document, new: &Document) -> Changes {
    let key = |doc: &Document| -> Vec<String> {
        (0..doc.blocks.len())
            .map(|ix| doc.source_of(ix).trim().to_string())
            .collect()
    };
    let (old_keys, new_keys) = (key(old), key(new));
    let mut marks: Vec<Mark> = (0..new.blocks.len()).map(|_| Mark::Same).collect();
    let mut removed = Vec::new();
    for op in capture_diff_slices(Algorithm::Patience, &old_keys, &new_keys) {
        let (tag, olds, news) = op.as_tag_tuple();
        match tag {
            DiffTag::Equal => {}
            DiffTag::Insert => news.for_each(|ix| marks[ix] = Mark::Added),
            DiffTag::Delete => removed.extend(olds.map(|ix| (news.start, ix))),
            DiffTag::Replace => {
                let paired = olds.len().min(news.len());
                for k in 0..paired {
                    let (o, n) = (olds.start + k, news.start + k);
                    let words = words_of(&old.blocks[o], &new.blocks[n]);
                    if words.is_none() {
                        removed.push((n, o));
                    }
                    marks[n] = Mark::Changed(words);
                }
                for mark in &mut marks[news.start + paired..news.end] {
                    *mark = Mark::Added;
                }
                for o in olds.start + paired..olds.end {
                    removed.push((news.end, o));
                }
            }
        }
    }
    Changes { marks, removed }
}

/// The word diff of two paragraphs or two headings of the same level, without formulas.
fn words_of(old: &Block, new: &Block) -> Option<Vec<Word>> {
    let (old, new) = match (old, new) {
        (Block::Paragraph(a), Block::Paragraph(b)) => (a, b),
        (Block::Heading(la, a), Block::Heading(lb, b)) if la == lb => (a, b),
        _ => return None,
    };
    let (old, new) = (tokens(old)?, tokens(new)?);
    let key = |tokens: &[(String, Marks)]| -> Vec<String> {
        tokens
            .iter()
            .map(|(t, _)| t.trim_end().to_string())
            .collect()
    };
    let mut words = Vec::new();
    for op in capture_diff_slices(Algorithm::Myers, &key(&old), &key(&new)) {
        let (tag, olds, news) = op.as_tag_tuple();
        let push = |words: &mut Vec<Word>, (text, marks): &(String, Marks), change| {
            words.push(Word {
                text: text.clone(),
                marks: marks.clone(),
                change,
            })
        };
        match tag {
            DiffTag::Equal => news.for_each(|ix| push(&mut words, &new[ix], Change::Same)),
            DiffTag::Insert => news.for_each(|ix| push(&mut words, &new[ix], Change::Inserted)),
            DiffTag::Delete => olds.for_each(|ix| push(&mut words, &old[ix], Change::Deleted)),
            DiffTag::Replace => {
                olds.for_each(|ix| push(&mut words, &old[ix], Change::Deleted));
                news.for_each(|ix| push(&mut words, &new[ix], Change::Inserted));
            }
        }
    }
    Some(words)
}

/// The words of `inlines`, each with its trailing space and its marks; none when a formula is
/// among them.
fn tokens(inlines: &[Inline]) -> Option<Vec<(String, Marks)>> {
    let mut out = Vec::new();
    for inline in inlines {
        let Inline::Text(text, marks) = inline else {
            return None;
        };
        for word in text.split_inclusive(char::is_whitespace) {
            out.push((word.replace('\n', " "), marks.clone()));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::parse;

    fn changes(old: &str, new: &str) -> Changes {
        compare(&parse(old, "#ffffff", None), &parse(new, "#ffffff", None))
    }

    #[test]
    fn a_section_added_is_added_and_the_rest_the_same() {
        let c = changes(
            "# Tomatoes\n\nSow in spring.\n",
            "# Tomatoes\n\nSow in spring.\n\n## Watering\n\nTwice a week.\n",
        );
        let added: Vec<bool> = c.marks.iter().map(|m| matches!(m, Mark::Added)).collect();
        assert_eq!(added, [false, false, true, true]);
        assert!(c.removed.is_empty());
        assert_eq!(c.stops(), [2]);
    }

    #[test]
    fn a_word_changed_in_a_paragraph_shows_as_words() {
        let c = changes("The oven is high.\n", "The oven is low.\n");
        let Mark::Changed(Some(words)) = &c.marks[0] else {
            panic!("not a word change")
        };
        let changed: Vec<(&str, Change)> = words
            .iter()
            .filter(|w| w.change != Change::Same)
            .map(|w| (w.text.as_str(), w.change))
            .collect();
        assert_eq!(
            changed,
            [("high.", Change::Deleted), ("low.", Change::Inserted)]
        );
        assert!(c.removed.is_empty());
    }

    #[test]
    fn a_block_removed_stands_before_the_block_that_followed_it() {
        let c = changes("# A\n\nGone.\n\n# B\n", "# A\n\n# B\n");
        assert_eq!(c.removed, [(1, 1)]);
        assert_eq!(c.stops(), [1]);
        assert_eq!(changes("x\n", "x\n").count(), 0);
    }

    #[test]
    fn a_block_that_changed_kind_keeps_the_old_one_struck_above() {
        let c = changes("Plain.\n", "- Plain.\n");
        assert!(matches!(c.marks[0], Mark::Changed(None)));
        assert_eq!(c.removed, [(0, 0)]);
    }
}
