//! An optimized owned slice type styled after "[Umbra strings]."
//!
//! This is similar to a `Box<[T]>` for `T = u8` or `T = u16` with a small-slice optimization and
//! further optimizations for `Eq` and prefix comparisons.
//!
//! Some background on the memory layout of `&[T]`/`Box<[T]>` is important. References to slices
//! are "fat pointers." They're _wider_ than a straight-up pointer to a memory location (hence
//! "fat") because they carry metadata along with the pointer. Imagine a tuple `(*const T,
//! Metadata)`. For slices this metadata is the slice's length - so you can say `my_slice.len()`
//! rather than passing around the length for every function call where you pass a slice. A
//! pointer is of size `usize` (by definition) and the length is also represented as a `usize`. On
//! a 64-bit machine (where `size_of::<usize>() == 8` - 8 bytes / 64 bits) this means that your
//! slice fat pointer takes 16 bytes to represent.
//!
//! `usize` is rather large to represent the size of a slice though. Your typical slice is very
//! likely to be much much smaller. Think of all of the bits, (no, bytes!) of the length `usize`
//! you waste per slice.
//!
//! "Umbra strings" or "German strings" or "German-style strings" (all the same thing) aim to take
//! advantage of the common cases of strings:
//!
//! * Strings are usually short.
//! * Strings are usually immutable.
//! * You usually care only about a small part of the string (prefix or equality).
//!
//! We'll take advantage of these common cases by (ab)using these wasted bytes. To do this we
//! steal bytes from the length `usize` and also introduce a small-string ("small-slice")
//! optimization (SSO).
//!
//! The first change is to represent the length with a `u16`. That's 6 bytes saved for future use,
//! with the restriction that the slice is limited to 2^16 (65,536) elements - more than enough
//! for our use-case. We'll use these six bytes to store a prefix of the slice instead and we can
//! use that prefix for fast equality checking between fellow Umbra slices.
//!
//! We have 6 bytes of the slice already stored in the prefix, so for the common case of short
//! strings let's just store the rest of the string in the struct and forget about the pointer.
//! So for, say, a UTF-8 encoded ASCII string with 14 or fewer characters we don't need pointer
//! indirection or allocation at all. The string contents are stored entirely in the struct
//! itself.
//!
//! ```text
//! +---------+---------------------------+-------------------------------------+
//! +   len   +          prefix                          suffix                 +
//! +---------+---------------------------+-------------------------------------+
//!     u16                            14x u8
//! ```
//!
//! For the rare longer slice we'll allocate the slice as a "thin" pointer. We already store the
//! length earlier in the struct so there's no need to use a fat pointer.
//!
//! ```text
//! +---------+---------------------------+-------------------------------------+
//! +   len   +          prefix           +              pointer                +
//! +---------+---------------------------+-------------------------------------+
//!     u16               6x u8                          8x u8
//! ```
//!
//! Note that the `pointer` here points to an allocation for the entire slice rather than just the
//! portion not covered by the `prefix` - this is important so that we can reconstruct the slice
//! type in contiguous memory (i.e. create a `&str` from a `UmbraSlice<u8>`) without reallocation.
//! This means that this slice type duplicates the prefix `T`s for larger slices. On the other
//! hand you avoid allocating any `T`s though for the small variant, so when this type is usually
//! short it should save a noticeable amount of memory.
//!
//! Also note that this type is cheaper to compare to itself (`Eq`) but a `UmbraSlice<u8>` is
//! slightly slower to compare to a `&[u8]` because the `len` must be read and the slice must be
//! reconstructed before comparison. (This requires an extra comparison operation.)
//!
//! For further reading: <https://the-mikedavis.github.io/posts/german-string-optimizations-in-spellbook/>.
//!
//! The `UmbraSlice` and `UmbraString` types have also been extracted to a crate should you wish
//! to use them: [`umbra_slice`](https://crates.io/crates/umbra_slice).
//!
//! [Umbra strings]: https://cedardb.com/blog/german_strings/

use core::{
    borrow::Borrow,
    fmt, hash,
    mem::{self, size_of, ManuallyDrop, MaybeUninit},
    ptr::{self, NonNull},
    slice,
};

use crate::alloc::{alloc, string::String};

// NOTE: ideally this type would be defined in `src/lib.rs` but that would mean leaking out the
// `prefix_len`/`suffix_len` `const fn`s below. As noted below the type, those should ideally
// not exist anyways.
use crate::Flag;
pub type FlagSlice = UmbraSlice<Flag, { prefix_len::<Flag>() }, { suffix_len::<Flag>() }>;

// NOTE: this module would be miles cleaner if the `generic_const_exprs` Rust feature were
// implemented. Without it we need to pass in the prefix and suffix lengths as const type
// parameters which is quite verbose. Don't fear, though the generics make my eyes bleed, it's
// a straightforward formula to determine the number of `T`s that can fit in the prefix and suffix
// sections of the type, as follows:

/// Computes the number of `T`s that can be stored in the prefix of the Umbra slice.
const fn prefix_len<T>() -> usize {
    // Remove 16 bits for the `len`.
    (size_of::<usize>() - size_of::<u16>()) / size_of::<T>()
}
const fn suffix_len<T>() -> usize {
    // Note that the size of the suffix field in the `Trailing` union below is always
    // `size_of::<usize>()`. This value is the number of `T`s that fit into that slot.
    size_of::<usize>() / size_of::<T>()
}

// Note: T is assumed to be u8/u16 (though i8/i16 is probably fine too). u32/i32 or larger is
// probably not worth it and the alignment wouldn't work out nicely since `len` is a `u16`.
#[repr(C)]
pub struct UmbraSlice<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> {
    len: u16,
    prefix: [MaybeUninit<T>; PREFIX_LEN],
    trailing: Trailing<T, SUFFIX_LEN>,
}

#[repr(C)]
union Trailing<T: Copy, const SUFFIX_LEN: usize> {
    suffix: [MaybeUninit<T>; SUFFIX_LEN],
    ptr: ManuallyDrop<NonNull<T>>,
}

impl<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize>
    UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN>
{
    /// The total number of `T`s that can be stored in `UmbraSlice` inline (i.e. without
    /// allocating).
    const INLINE_LEN: u16 = (PREFIX_LEN + SUFFIX_LEN) as u16;

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[T] {
        let ptr = if self.len <= Self::INLINE_LEN {
            let ptr = ptr_from_ref(self);
            unsafe { ptr::addr_of!((*ptr).prefix) }.cast()
        } else {
            unsafe { self.trailing.ptr }.as_ptr()
        };

        unsafe { slice::from_raw_parts(ptr, self.len()) }
    }

    fn copy_slice(source: &[T]) -> NonNull<T> {
        let layout = Self::layout(source.len());
        let nullable = unsafe { alloc::alloc(layout) };
        let ptr = match NonNull::new(nullable) {
            Some(ptr) => ptr.cast(),
            None => alloc::handle_alloc_error(layout),
        };
        unsafe {
            ptr::copy_nonoverlapping(source.as_ptr(), ptr.as_ptr(), source.len());
        }
        ptr
    }

    fn layout(len: usize) -> alloc::Layout {
        alloc::Layout::array::<T>(len)
            .expect("a valid layout for an array")
            .pad_to_align()
    }
}

impl<T: Copy + Ord + Eq, const PREFIX_LEN: usize, const SUFFIX_LEN: usize>
    UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN>
{
    /// Checks whether the given element is a member of the slice.
    ///
    /// The slice must be sorted.
    pub fn sorted_contains(&self, t: &T) -> bool {
        if self.len <= Self::INLINE_LEN {
            let ptr = unsafe { ptr::addr_of!((*ptr_from_ref(self)).prefix) }.cast();
            let slice = unsafe { slice::from_raw_parts(ptr, self.len()) };
            // `slice::contains` is ever so slightly faster than a binary search on a small slice.
            slice.contains(t)
        } else {
            let ptr = unsafe { self.trailing.ptr }.as_ptr();
            let slice = unsafe { slice::from_raw_parts(ptr, self.len()) };
            slice.binary_search(t).is_ok()
        }
    }
}

impl<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> Drop
    for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN>
{
    fn drop(&mut self) {
        if self.len > Self::INLINE_LEN {
            let ptr = unsafe { self.trailing.ptr }.as_ptr();
            let layout = Self::layout(self.len());
            unsafe { alloc::dealloc(ptr.cast(), layout) }
        }
    }
}

impl<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> Clone
    for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN>
{
    fn clone(&self) -> Self {
        let trailing = if self.len <= Self::INLINE_LEN {
            let suffix = unsafe { self.trailing.suffix };
            Trailing { suffix }
        } else {
            let ptr = ManuallyDrop::new(Self::copy_slice(self.as_slice()));
            Trailing { ptr }
        };

        Self {
            len: self.len,
            prefix: self.prefix,
            trailing,
        }
    }
}

impl<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> Default
    for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN>
{
    fn default() -> Self {
        Self {
            len: 0,
            prefix: [MaybeUninit::zeroed(); PREFIX_LEN],
            trailing: Trailing {
                suffix: [MaybeUninit::zeroed(); SUFFIX_LEN],
            },
        }
    }
}

/// An error used in the `TryFrom` implementation for `UmbraSlice` which rejects input slices
/// which are too long.
///
/// `UmbraSlice` only supports input slices with length smaller than or equal to `u16::MAX`.
#[derive(Debug)]
pub struct TooLongError;

impl fmt::Display for TooLongError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("input slice was too long (longer than u16::MAX)")
    }
}

impl<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> TryFrom<&[T]>
    for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN>
{
    type Error = TooLongError;

    fn try_from(source: &[T]) -> Result<Self, Self::Error> {
        // This is a polyfill around the unstable `core::mem::copy_from_slice`. The return value
        // has been removed since it is not interesting to this use-case and requires another
        // unstable function (`MaybeUninit::slice_assume_init_mut`).
        fn copy_to_slice<U: Copy>(this: &mut [MaybeUninit<U>], src: &[U]) {
            // SAFETY: &[U] and &[MaybeUninit<U>] have the same layout.
            let uninit_src = unsafe { mem::transmute::<&[U], &[MaybeUninit<U>]>(src) };
            this.copy_from_slice(uninit_src);
        }

        if source.len() > u16::MAX as usize {
            return Err(TooLongError);
        }

        let len = source.len();
        let mut prefix = [MaybeUninit::zeroed(); PREFIX_LEN];

        if len as u16 <= Self::INLINE_LEN {
            let mut suffix = [MaybeUninit::zeroed(); SUFFIX_LEN];
            if len <= PREFIX_LEN {
                copy_to_slice(&mut prefix[..len], source);
            } else {
                copy_to_slice(&mut prefix, &source[..PREFIX_LEN]);
                copy_to_slice(&mut suffix[..len - PREFIX_LEN], &source[PREFIX_LEN..]);
            }

            Ok(Self {
                len: len as u16,
                prefix,
                trailing: Trailing { suffix },
            })
        } else {
            copy_to_slice(&mut prefix, &source[..PREFIX_LEN]);
            let ptr = ManuallyDrop::new(Self::copy_slice(source));

            Ok(Self {
                len: len as u16,
                prefix,
                trailing: Trailing { ptr },
            })
        }
    }
}

// The fun implementation. This is the optimization that makes `Eq` fast.
impl<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> PartialEq<Self>
    for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN>
{
    fn eq(&self, other: &Self) -> bool {
        // Compare the length+prefix usize as one big chunk in a single comparison operation.
        // SAFETY: In `Default` and `TryFrom`, the `prefix` field is created with
        // `MaybeUninit::zeroed` memory, so even if the slice has fewer than `PREFIX_LEN`
        // elements, comparing the uninitialized memory is defined behavior.
        let self_len_and_prefix = ptr_from_ref(self).cast::<usize>();
        let other_len_and_prefix = ptr_from_ref(other).cast::<usize>();
        if unsafe { *self_len_and_prefix != *other_len_and_prefix } {
            return false;
        }

        // If the prefixes compare equal, check the suffixes too.
        // Because of the equality check above, `self.len` is now guaranteed to be the same
        // as `other.len`.
        if self.len <= Self::INLINE_LEN {
            // For inline slices compare the entire suffix as a single `usize` comparison like
            // we did for the len+prefix fields.
            // SAFETY: as noted above for prefixes, we initialized the suffix with
            // `MaybeUninit::zeroed()`. The zeroed bit pattern might not be valid for `T` but that
            // doesn't really matter: comparing zeroed bits for equality is defined behavior and
            // is accurate when `T` is `Copy`.
            let self_ptr = ptr_from_ref(self);
            let self_suffix = unsafe { ptr::addr_of!((*self_ptr).trailing.suffix) }.cast::<usize>();
            let other_ptr = ptr_from_ref(other);
            let other_suffix =
                unsafe { ptr::addr_of!((*other_ptr).trailing.suffix) }.cast::<usize>();

            unsafe { *self_suffix == *other_suffix }
        } else {
            let suffix_len = self.len() - PREFIX_LEN;
            let n_bytes = suffix_len * size_of::<T>();
            // SAFETY: The lengths of the slices compared equal above and `T` is bound to `Copy`
            // so bytewise equality is correct.
            unsafe {
                memcmp(
                    self.trailing.ptr.as_ptr().add(PREFIX_LEN).cast(),
                    other.trailing.ptr.as_ptr().add(PREFIX_LEN).cast(),
                    n_bytes,
                ) == 0
            }
        }
    }
}

// Snipped from `library/core/src/slice/cmp.rs` in the standard library:
extern "C" {
    /// Calls implementation provided memcmp.
    ///
    /// Interprets the data as u8.
    ///
    /// Returns 0 for equal, < 0 for less than and > 0 for greater
    /// than.
    fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> core::ffi::c_int;
}

// This is a polyfill for `core::ptr::from_ref` introduced in Rust 1.76.0.
const fn ptr_from_ref<T: ?Sized>(r: &T) -> *const T {
    r
}

impl<T: Copy + Eq, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> Eq
    for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN>
{
}

unsafe impl<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> Send
    for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN>
{
}
unsafe impl<T: Copy, const PREFIX_LEN: usize, const SUFFIX_LEN: usize> Sync
    for UmbraSlice<T, PREFIX_LEN, SUFFIX_LEN>
{
}

/// A UTF-8 string representation, like `String` but backed by an Umbra slice.
///
/// This is closer to a `Box<str>` than a `String` since the allocation is not resizable.
/// The contents are assumed to be valid UTF-8 by construction.
#[derive(Clone, PartialEq, Eq)]
pub struct UmbraString(U8UmbraSlice);

type U8UmbraSlice = UmbraSlice<u8, { prefix_len::<u8>() }, { suffix_len::<u8>() }>;

impl UmbraString {
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    pub fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(self.as_bytes()) }
    }
}

impl AsRef<str> for UmbraString {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for UmbraString {
    fn borrow(&self) -> &str {
        self.as_ref()
    }
}

impl hash::Hash for UmbraString {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        // Note: `Hash for UmbraString` must have the same outputs as `Hash for str` in order for
        // our hash bag lookup to work.
        self.as_ref().hash(state)
    }
}

impl PartialEq<str> for UmbraString {
    fn eq(&self, other: &str) -> bool {
        // No special optimizations here. We pay the cost of converting to bytes and compare as
        // bytes. If anything this is slightly slower than `Eq for str` since we need the extra
        // comparison to construct the byte slice.
        self.as_bytes() == other.as_bytes()
    }
}

impl fmt::Debug for UmbraString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl From<&str> for UmbraString {
    fn from(s: &str) -> Self {
        // Inputs over u16::MAX bytes are rejected at the dictionary parse boundary;
        // internal callers are guaranteed to pass bounded strings.
        assert!(
            s.len() <= u16::MAX as usize,
            "UmbraString::from called with input longer than u16::MAX bytes"
        );
        Self(s.as_bytes().try_into().unwrap())
    }
}

impl From<String> for UmbraString {
    fn from(s: String) -> Self {
        s.as_str().into()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    type U16UmbraSlice = UmbraSlice<u16, { prefix_len::<u16>() }, { suffix_len::<u16>() }>;

    #[test]
    fn sizing() {
        use crate::alloc::boxed::Box;

        assert_eq!(size_of::<U16UmbraSlice>(), 2 * size_of::<usize>());
        assert_eq!(size_of::<UmbraString>(), 2 * size_of::<usize>());
        assert_eq!(size_of::<Box<str>>(), 2 * size_of::<usize>());
        assert_eq!(size_of::<Box<[crate::Flag]>>(), 2 * size_of::<usize>());
    }

    impl fmt::Debug for U16UmbraSlice {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            if self.len <= Self::INLINE_LEN {
                f.debug_struct("U16UmbraSlice")
                    .field("len", &self.len)
                    .field("prefix", unsafe {
                        &mem::transmute::<&[MaybeUninit<u16>], &[u16]>(&self.prefix)
                    })
                    .field("suffix", unsafe {
                        &mem::transmute::<&[MaybeUninit<u16>], &[u16]>(&self.trailing.suffix)
                    })
                    .finish()
            } else {
                todo!()
            }
        }
    }

    #[test]
    fn eq() {
        assert_eq!(
            U16UmbraSlice::try_from([1u16].as_slice()).unwrap(),
            U16UmbraSlice::try_from([1u16].as_slice()).unwrap(),
        );
        assert_eq!(UmbraString::from(""), UmbraString::from(""));
        assert_eq!(UmbraString::from("foo"), UmbraString::from("foo"),);
    }

    #[test]
    fn partial_eq_str() {
        // Small (prefix)
        assert_eq!(UmbraString::from("hello"), *"hello");
        // Medium (prefix+suffix)
        assert_eq!(UmbraString::from("foobarbaz"), *"foobarbaz");
        // Large (allocated buffer)
        assert_eq!(UmbraString::from("hellohellohello"), *"hellohellohello");
    }

    #[test]
    fn clone() {
        let s = UmbraString::from("hellohellohello");
        assert_eq!(s.clone(), s);
        assert_eq!(s.clone(), *"hellohellohello");

        let s = UmbraString::from("foo");
        assert_eq!(s.clone(), s);
        assert_eq!(s.clone(), *"foo");
    }

    #[test]
    fn hash() {
        use core::hash::{BuildHasher, Hash, Hasher};
        fn hash_once<T: Hash, S: BuildHasher>(hash_builder: &S, val: &T) -> u64 {
            let mut state = hash_builder.build_hasher();
            val.hash(&mut state);
            state.finish()
        }
        let hasher = crate::DefaultHashBuilder::default();

        let s = UmbraString::from("hellohellohello");
        assert_eq!(hash_once(&hasher, &s), hash_once(&hasher, &s));
        assert_eq!(hash_once(&hasher, &s), hash_once(&hasher, &s.clone()));
        assert_eq!(
            hash_once(&hasher, &s),
            hash_once(&hasher, &UmbraString::from("hellohellohello"))
        );
        assert_eq!(
            hash_once(&hasher, &s),
            hash_once(&hasher, &"hellohellohello")
        );
    }
}
