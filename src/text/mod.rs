//! Text diffing utilities.
use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

mod abstraction;
#[cfg(feature = "inline")]
mod inline;
mod utils;

pub use self::abstraction::{DiffableStr, DiffableStrRef};
#[cfg(feature = "inline")]
pub use self::inline::InlineChange;

use self::utils::{upper_seq_ratio, QuickSeqRatio};
use crate::udiff::UnifiedDiff;
use crate::{capture_diff_slices, get_diff_ratio, group_diff_ops, Algorithm, Change, DiffOp};

/// A builder type config for more complex uses of [`TextDiff`].
///
/// Requires the `text` feature.
#[derive(Clone, Debug)]
pub struct TextDiffConfig {
    algorithm: Algorithm,
    newline_terminated: Option<bool>,
}

impl Default for TextDiffConfig {
    fn default() -> TextDiffConfig {
        TextDiffConfig {
            algorithm: Algorithm::default(),
            newline_terminated: None,
        }
    }
}

impl TextDiffConfig {
    /// Changes the algorithm.
    ///
    /// The default algorithm is [`Algorithm::Myers`].
    pub fn algorithm(&mut self, alg: Algorithm) -> &mut Self {
        self.algorithm = alg;
        self
    }

    /// Changes the newline termination flag.
    ///
    /// The default is automatic based on input.  This flag controls the
    /// behavior of [`TextDiff::iter_changes`] and unified diff generation
    /// with regards to newlines.  When the flag is set to `false` (which
    /// is the default) then newlines are added.  Otherwise the newlines
    /// from the source sequences are reused.
    pub fn newline_terminated(&mut self, yes: bool) -> &mut Self {
        self.newline_terminated = Some(yes);
        self
    }

    /// Creates a diff of lines.
    ///
    /// This splits the text `old` and `new` into lines preserving newlines
    /// in the input.
    pub fn diff_lines<'old, 'new, 'bufs, T: DiffableStrRef + ?Sized>(
        &self,
        old: &'old T,
        new: &'new T,
    ) -> TextDiff<'old, 'new, 'bufs, T::Output> {
        self.diff(
            Cow::Owned(old.as_diffable_str().tokenize_lines()),
            Cow::Owned(new.as_diffable_str().tokenize_lines()),
            true,
        )
    }

    /// Creates a diff of words.
    ///
    /// This splits the text into words and whitespace.
    pub fn diff_words<'old, 'new, 'bufs, T: DiffableStrRef + ?Sized>(
        &self,
        old: &'old T,
        new: &'new T,
    ) -> TextDiff<'old, 'new, 'bufs, T::Output> {
        self.diff(
            Cow::Owned(old.as_diffable_str().tokenize_words()),
            Cow::Owned(new.as_diffable_str().tokenize_words()),
            false,
        )
    }

    /// Creates a diff of characters.
    pub fn diff_chars<'old, 'new, 'bufs, T: DiffableStrRef + ?Sized>(
        &self,
        old: &'old T,
        new: &'new T,
    ) -> TextDiff<'old, 'new, 'bufs, T::Output> {
        self.diff(
            Cow::Owned(old.as_diffable_str().tokenize_chars()),
            Cow::Owned(new.as_diffable_str().tokenize_chars()),
            false,
        )
    }

    /// Creates a diff of unicode words.
    ///
    /// This splits the text into words according to unicode rules.  This is
    /// generally recommended over [`TextDiffConfig::diff_words`] but
    /// requires a dependency.
    ///
    /// This requires the `unicode` feature.
    #[cfg(feature = "unicode")]
    pub fn diff_unicode_words<'old, 'new, 'bufs, T: DiffableStrRef + ?Sized>(
        &self,
        old: &'old T,
        new: &'new T,
    ) -> TextDiff<'old, 'new, 'bufs, T::Output> {
        self.diff(
            Cow::Owned(old.as_diffable_str().tokenize_unicode_words()),
            Cow::Owned(new.as_diffable_str().tokenize_unicode_words()),
            false,
        )
    }

    /// Creates a diff of graphemes.
    ///
    /// This requires the `unicode` feature.
    #[cfg(feature = "unicode")]
    pub fn diff_graphemes<'old, 'new, 'bufs, T: DiffableStrRef + ?Sized>(
        &self,
        old: &'old T,
        new: &'new T,
    ) -> TextDiff<'old, 'new, 'bufs, T::Output> {
        self.diff(
            Cow::Owned(old.as_diffable_str().tokenize_graphemes()),
            Cow::Owned(new.as_diffable_str().tokenize_graphemes()),
            false,
        )
    }

    /// Creates a diff of arbitrary slices.
    pub fn diff_slices<'old, 'new, 'bufs, T: DiffableStr + ?Sized>(
        &self,
        old: &'bufs [&'old T],
        new: &'bufs [&'new T],
    ) -> TextDiff<'old, 'new, 'bufs, T> {
        self.diff(Cow::Borrowed(old), Cow::Borrowed(new), false)
    }

    fn diff<'old, 'new, 'bufs, T: DiffableStr + ?Sized>(
        &self,
        old: Cow<'bufs, [&'old T]>,
        new: Cow<'bufs, [&'new T]>,
        newline_terminated: bool,
    ) -> TextDiff<'old, 'new, 'bufs, T> {
        let ops = capture_diff_slices(self.algorithm, &old, &new);
        TextDiff {
            old,
            new,
            ops,
            newline_terminated: self.newline_terminated.unwrap_or(newline_terminated),
            algorithm: self.algorithm,
        }
    }
}

/// Captures diff op codes for textual diffs.
///
/// The exact diff behavior is depending on the underlying [`DiffableStr`].
/// For instance diffs on bytes and strings are slightly different.  You can
/// create a text diff from constructors such as [`TextDiff::from_lines`] or
/// the [`TextDiffConfig`] created by [`TextDiff::configure`].
///
/// Requires the `text` feature.
pub struct TextDiff<'old, 'new, 'bufs, T: DiffableStr + ?Sized> {
    old: Cow<'bufs, [&'old T]>,
    new: Cow<'bufs, [&'new T]>,
    ops: Vec<DiffOp>,
    newline_terminated: bool,
    algorithm: Algorithm,
}

impl<'old, 'new, 'bufs> TextDiff<'old, 'new, 'bufs, str> {
    /// Configures a text differ before diffing.
    pub fn configure() -> TextDiffConfig {
        TextDiffConfig::default()
    }

    /// Creates a diff of lines.
    ///
    /// Equivalent to `TextDiff::configure().diff_lines(old, new)`.
    pub fn from_lines<T: DiffableStrRef + ?Sized>(
        old: &'old T,
        new: &'new T,
    ) -> TextDiff<'old, 'new, 'bufs, T::Output> {
        TextDiff::configure().diff_lines(old, new)
    }

    /// Creates a diff of words.
    ///
    /// Equivalent to `TextDiff::configure().diff_words(old, new)`.
    pub fn from_words<T: DiffableStrRef + ?Sized>(
        old: &'old T,
        new: &'new T,
    ) -> TextDiff<'old, 'new, 'bufs, T::Output> {
        TextDiff::configure().diff_words(old, new)
    }

    /// Creates a diff of chars.
    ///
    /// Equivalent to `TextDiff::configure().diff_chars(old, new)`.
    pub fn from_chars<T: DiffableStrRef + ?Sized>(
        old: &'old T,
        new: &'new T,
    ) -> TextDiff<'old, 'new, 'bufs, T::Output> {
        TextDiff::configure().diff_chars(old, new)
    }

    /// Creates a diff of unicode words.
    ///
    /// Equivalent to `TextDiff::configure().diff_unicode_words(old, new)`.
    ///
    /// This requires the `unicode` feature.
    #[cfg(feature = "unicode")]
    pub fn from_unicode_words<T: DiffableStrRef + ?Sized>(
        old: &'old T,
        new: &'new T,
    ) -> TextDiff<'old, 'new, 'bufs, T::Output> {
        TextDiff::configure().diff_unicode_words(old, new)
    }

    /// Creates a diff of graphemes.
    ///
    /// Equivalent to `TextDiff::configure().diff_graphemes(old, new)`.
    ///
    /// This requires the `unicode` feature.
    #[cfg(feature = "unicode")]
    pub fn from_graphemes<T: DiffableStrRef + ?Sized>(
        old: &'old T,
        new: &'new T,
    ) -> TextDiff<'old, 'new, 'bufs, T::Output> {
        TextDiff::configure().diff_graphemes(old, new)
    }
}

impl<'old, 'new, 'bufs, T: DiffableStr + ?Sized + 'old + 'new> TextDiff<'old, 'new, 'bufs, T> {
    /// Creates a diff of arbitrary slices.
    ///
    /// Equivalent to `TextDiff::configure().diff_slices(old, new)`.
    pub fn from_slices(
        old: &'bufs [&'old T],
        new: &'bufs [&'new T],
    ) -> TextDiff<'old, 'new, 'bufs, T> {
        TextDiff::configure().diff_slices(old, new)
    }

    /// The name of the algorithm that created the diff.
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// Returns `true` if items in the slice are newline terminated.
    ///
    /// This flag is used by the unified diff writer to determine if extra
    /// newlines have to be added.
    pub fn newline_terminated(&self) -> bool {
        self.newline_terminated
    }

    /// Returns all old slices.
    pub fn old_slices(&self) -> &[&'old T] {
        &self.old
    }

    /// Returns all new slices.
    pub fn new_slices(&self) -> &[&'new T] {
        &self.new
    }

    /// Return a measure of the sequences' similarity in the range `0..=1`.
    ///
    /// A ratio of `1.0` means the two sequences are a complete match, a
    /// ratio of `0.0` would indicate completely distinct sequences.
    ///
    /// ```rust
    /// # use similar::TextDiff;
    /// let diff = TextDiff::from_chars("abcd", "bcde");
    /// assert_eq!(diff.ratio(), 0.75);
    /// ```
    pub fn ratio(&self) -> f32 {
        get_diff_ratio(self.ops(), self.old.len(), self.new.len())
    }

    /// Iterates over the changes the op expands to.
    ///
    /// This method is a convenient way to automatically resolve the different
    /// ways in which a change could be encoded (insert/delete vs replace), look
    /// up the value from the appropriate slice and also handle correct index
    /// handling.
    pub fn iter_changes<'x, 'slf>(
        &'slf self,
        op: &DiffOp,
    ) -> impl Iterator<Item = Change<'x, T>> + 'slf
    where
        'x: 'slf,
        'old: 'x,
        'new: 'x,
    {
        op.iter_changes(self.old_slices(), self.new_slices())
    }

    /// Returns the captured diff ops.
    pub fn ops(&self) -> &[DiffOp] {
        &self.ops
    }

    /// Isolate change clusters by eliminating ranges with no changes.
    ///
    /// This is equivalent to calling [`group_diff_ops`] on [`TextDiff::ops`].
    pub fn grouped_ops(&self, n: usize) -> Vec<Vec<DiffOp>> {
        group_diff_ops(self.ops().to_vec(), n)
    }

    /// Flattens out the diff into all changes.
    ///
    /// This is a shortcut for combining [`TextDiff::ops`] with
    /// [`TextDiff::iter_changes`].
    pub fn iter_all_changes<'x, 'slf>(&'slf self) -> impl Iterator<Item = Change<'x, T>> + 'slf
    where
        'x: 'slf,
        'old: 'x,
        'new: 'x,
    {
        // unclear why this needs Box::new here.  It seems to infer some really
        // odd lifetimes I can't figure out how to work with.
        Box::new(self.ops().iter().flat_map(move |op| self.iter_changes(&op)))
            as Box<dyn Iterator<Item = _>>
    }

    /// Utility to return a unified diff formatter.
    pub fn unified_diff<'diff>(&'diff self) -> UnifiedDiff<'diff, 'old, 'new, 'bufs, T> {
        UnifiedDiff::from_text_diff(self)
    }

    /// Iterates over the changes the op expands to with inline emphasis.
    ///
    /// This is very similar to [`TextDiff::iter_changes`] but it performs a second
    /// level diff on adjacent line replacements.  The exact behavior of
    /// this function with regards to how it detects those inline changes
    /// is currently not defined and will likely change over time.
    #[cfg(feature = "inline")]
    pub fn iter_inline_changes<'x, 'slf>(
        &'slf self,
        op: &DiffOp,
    ) -> impl Iterator<Item = InlineChange<'x, T>> + 'slf
    where
        'x: 'slf,
        'old: 'x,
        'new: 'x,
    {
        inline::iter_inline_changes(self, op)
    }
}

/// Use the text differ to find `n` close matches.
///
/// `cutoff` defines the threshold which needs to be reached for a word
/// to be considered similar.  See [`TextDiff::ratio`] for more information.
///
/// ```
/// # use similar::get_close_matches;
/// let matches = get_close_matches(
///     "appel",
///     &["ape", "apple", "peach", "puppy"][..],
///     3,
///     0.6
/// );
/// assert_eq!(matches, vec!["apple", "ape"]);
/// ```
///
/// Requires the `text` feature.
pub fn get_close_matches<'a, T: DiffableStr + ?Sized>(
    word: &T,
    possibilities: &[&'a T],
    n: usize,
    cutoff: f32,
) -> Vec<&'a T> {
    let mut matches = BinaryHeap::new();
    let seq1 = word.tokenize_chars();
    let quick_ratio = QuickSeqRatio::new(&seq1);

    for &possibility in possibilities {
        let seq2 = possibility.tokenize_chars();

        if upper_seq_ratio(&seq1, &seq2) < cutoff || quick_ratio.calc(&seq2) < cutoff {
            continue;
        }

        let diff = TextDiff::from_slices(&seq1, &seq2);
        let ratio = diff.ratio();
        if ratio >= cutoff {
            // we're putting the word itself in reverse in so that matches with
            // the same ratio are ordered lexicographically.
            matches.push(((ratio * u32::MAX as f32) as u32, Reverse(possibility)));
        }
    }

    let mut rv = vec![];
    for _ in 0..n {
        if let Some((_, elt)) = matches.pop() {
            rv.push(elt.0);
        } else {
            break;
        }
    }

    rv
}

#[test]
fn test_captured_ops() {
    let diff = TextDiff::from_lines(
        "Hello World\nsome stuff here\nsome more stuff here\n",
        "Hello World\nsome amazing stuff here\nsome more stuff here\n",
    );
    insta::assert_debug_snapshot!(&diff.ops());
}

#[test]
fn test_captured_word_ops() {
    let diff = TextDiff::from_words(
        "Hello World\nsome stuff here\nsome more stuff here\n",
        "Hello World\nsome amazing stuff here\nsome more stuff here\n",
    );
    let changes = diff
        .ops()
        .iter()
        .flat_map(|op| diff.iter_changes(op))
        .collect::<Vec<_>>();
    insta::assert_debug_snapshot!(&changes);
}

#[test]
fn test_unified_diff() {
    let diff = TextDiff::from_lines(
        "Hello World\nsome stuff here\nsome more stuff here\n",
        "Hello World\nsome amazing stuff here\nsome more stuff here\n",
    );
    assert_eq!(diff.newline_terminated(), true);
    insta::assert_snapshot!(&diff
        .unified_diff()
        .context_radius(3)
        .header("old", "new")
        .to_string());
}

#[test]
fn test_line_ops() {
    let a = "Hello World\nsome stuff here\nsome more stuff here\n";
    let b = "Hello World\nsome amazing stuff here\nsome more stuff here\n";
    let diff = TextDiff::from_lines(a, b);
    assert_eq!(diff.newline_terminated(), true);
    let changes = diff
        .ops()
        .iter()
        .flat_map(|op| diff.iter_changes(op))
        .collect::<Vec<_>>();
    insta::assert_debug_snapshot!(&changes);

    #[cfg(feature = "bytes")]
    {
        let byte_diff = TextDiff::from_lines(a.as_bytes(), b.as_bytes());
        let byte_changes = byte_diff
            .ops()
            .iter()
            .flat_map(|op| byte_diff.iter_changes(op))
            .collect::<Vec<_>>();
        for (change, byte_change) in changes.iter().zip(byte_changes.iter()) {
            assert_eq!(change.to_string_lossy(), byte_change.to_string_lossy());
        }
    }
}

#[test]
fn test_virtual_newlines() {
    let diff = TextDiff::from_lines("a\nb", "a\nc\n");
    assert_eq!(diff.newline_terminated(), true);
    let changes = diff
        .ops()
        .iter()
        .flat_map(|op| diff.iter_changes(op))
        .collect::<Vec<_>>();
    insta::assert_debug_snapshot!(&changes);
}

#[test]
fn test_char_diff() {
    let diff = TextDiff::from_chars("Hello World", "Hallo Welt");
    insta::assert_debug_snapshot!(diff.ops());

    #[cfg(feature = "bytes")]
    {
        let byte_diff = TextDiff::from_chars("Hello World".as_bytes(), "Hallo Welt".as_bytes());
        assert_eq!(diff.ops(), byte_diff.ops());
    }
}

#[test]
fn test_ratio() {
    let diff = TextDiff::from_chars("abcd", "bcde");
    assert_eq!(diff.ratio(), 0.75);
    let diff = TextDiff::from_chars("", "");
    assert_eq!(diff.ratio(), 1.0);
}

#[test]
fn test_get_close_matches() {
    let matches = get_close_matches("appel", &["ape", "apple", "peach", "puppy"][..], 3, 0.6);
    assert_eq!(matches, vec!["apple", "ape"]);
    let matches = get_close_matches(
        "hulo",
        &[
            "hi", "hulu", "hali", "hoho", "amaz", "zulo", "blah", "hopp", "uulo", "aulo",
        ][..],
        5,
        0.7,
    );
    assert_eq!(matches, vec!["aulo", "hulu", "uulo", "zulo"]);
}

#[test]
fn test_lifetimes_on_iter() {
    fn diff_lines<'x, T>(old: &'x T, new: &'x T) -> Vec<Change<'x, T::Output>>
    where
        T: DiffableStrRef + ?Sized,
    {
        TextDiff::from_lines(old, new).iter_all_changes().collect()
    }

    let a = "1\n2\n3\n".to_string();
    let b = "1\n99\n3\n".to_string();
    let changes = diff_lines(&a, &b);
    insta::assert_debug_snapshot!(&changes);
}
