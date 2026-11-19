//! Plain-text messages in pieces for translation (#240).
//!
//! A paragraph whose lines the sender wrapped at a fixed width is one piece,
//! so a sentence travels whole and comes back as one line the reader wraps
//! again. Other lines, such as a list, an address or a signature, are a
//! piece each. Quote marks (`>`) stay out of the pieces and go back in
//! front of the translation.

use super::document::{has_words, unescape};
use super::{Piece, Slot};

/// The shortest line that counts as wrapped. Mail is wrapped at 72 to 78
/// characters (RFC 5322 section 2.1.1 asks for at most 78), and a wrapped
/// line is rarely much shorter than that.
const WRAPPED: usize = 40;

#[derive(Debug)]
struct Line {
    /// The quote marks and the space after them, as written.
    quote: String,
    /// The line after the quote marks, indentation included.
    content: String,
}

#[derive(Debug)]
enum Block {
    /// A line written back as it was: blank, without words, or one that
    /// isn't translated.
    Kept(usize),
    /// Lines translated as one piece and written as one line.
    Joined(Vec<usize>),
    /// A line translated on its own.
    Single(usize),
}

pub(crate) struct TextDocument {
    lines: Vec<Line>,
    blocks: Vec<Block>,
}

fn split_quote(line: &str) -> (&str, &str) {
    let mut end = 0;
    let mut seen = false;
    for (index, c) in line.char_indices() {
        match c {
            '>' => {
                seen = true;
                end = index + 1;
            }
            ' ' if seen => end = index + 1,
            _ => break,
        }
    }
    line.split_at(end)
}

/// A line that starts an item of a list: a bullet or a number.
fn is_item(content: &str) -> bool {
    let trimmed = content.trim_start();
    if trimmed.starts_with(['-', '*', '•', '·', '–', '+']) {
        return true;
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && trimmed[digits..].starts_with(['.', ')'])
}

/// The quote depth of a line, so `> >` and `>>` count the same.
fn depth(quote: &str) -> usize {
    quote.matches('>').count()
}

impl TextDocument {
    pub(crate) fn parse(text: &str) -> Self {
        let lines: Vec<Line> = text
            .split('\n')
            .map(|line| {
                let line = line.strip_suffix('\r').unwrap_or(line);
                let (quote, content) = split_quote(line);
                Line {
                    quote: quote.to_owned(),
                    content: content.to_owned(),
                }
            })
            .collect();
        let mut blocks = Vec::new();
        let mut index = 0;
        while index < lines.len() {
            let line = &lines[index];
            // The signature separator (RFC 3676 section 4.3) and lines
            // without words stay as they are.
            if !has_words(&line.content) || line.content == "-- " {
                blocks.push(Block::Kept(index));
                index += 1;
                continue;
            }
            let mut end = index + 1;
            while end < lines.len()
                && depth(&lines[end].quote) == depth(&line.quote)
                && has_words(&lines[end].content)
                && lines[end].content != "-- "
                && !is_item(&lines[end].content)
            {
                end += 1;
            }
            let paragraph: Vec<usize> = (index..end).collect();
            let wrapped = paragraph.len() > 1
                && !is_item(&line.content)
                && paragraph[..paragraph.len() - 1]
                    .iter()
                    .all(|&i| lines[i].content.trim_end().chars().count() >= WRAPPED);
            if wrapped {
                blocks.push(Block::Joined(paragraph));
            } else {
                blocks.extend(paragraph.into_iter().map(Block::Single));
            }
            index = end;
        }
        TextDocument { lines, blocks }
    }

    pub(crate) fn pieces(&self) -> Vec<Piece> {
        let mut pieces = Vec::new();
        for (index, block) in self.blocks.iter().enumerate() {
            let text = match block {
                Block::Kept(_) => continue,
                Block::Joined(lines) => lines
                    .iter()
                    .map(|&i| self.lines[i].content.trim())
                    .collect::<Vec<_>>()
                    .join(" "),
                Block::Single(i) => self.lines[*i].content.trim().to_owned(),
            };
            let mut escaped = String::with_capacity(text.len());
            super::document::escape_piece(&text, &mut escaped);
            pieces.push(Piece {
                text: escaped,
                tagged: false,
                slot: Slot::Line(index),
            });
        }
        pieces
    }

    /// The message with each piece's translation, at the same index, in its
    /// place.
    pub(crate) fn translated(&self, pieces: &[Piece], translations: &[String]) -> String {
        let mut by_block = std::collections::BTreeMap::new();
        for (piece, text) in pieces.iter().zip(translations) {
            if let Slot::Line(block) = piece.slot
                && !text.trim().is_empty()
            {
                by_block.insert(block, unescape(text.trim()));
            }
        }
        let mut out: Vec<String> = Vec::with_capacity(self.lines.len());
        for (index, block) in self.blocks.iter().enumerate() {
            let translation = by_block.get(&index);
            match (block, translation) {
                (Block::Kept(i), _) => out.push(self.whole(*i)),
                (Block::Joined(lines), Some(text)) => {
                    out.push(format!("{}{}", self.lines[lines[0]].quote, text));
                }
                (Block::Joined(lines), None) => out.extend(lines.iter().map(|&i| self.whole(i))),
                (Block::Single(i), Some(text)) => {
                    let line = &self.lines[*i];
                    let indent =
                        &line.content[..line.content.len() - line.content.trim_start().len()];
                    out.push(format!("{}{indent}{text}", line.quote));
                }
                (Block::Single(i), None) => out.push(self.whole(*i)),
            }
        }
        out.join("\n")
    }

    fn whole(&self, index: usize) -> String {
        let line = &self.lines[index];
        format!("{}{}", line.quote, line.content)
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
