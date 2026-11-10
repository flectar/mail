use core::{
    borrow::Borrow,
    fmt::Debug,
    hash::{BuildHasher, Hash},
};

use hashbrown::hash_table::{self, HashTable};

/// A collection of key-value pairs - similar to a HashMap - which allows for duplicate keys.
///
/// The name is inspired by Erlang's ETS bag table type which also allows duplicate records.
/// Entire key-value pairs may be duplicated. Conceptually this is a lot like
/// `HashMap<K, Vec<V>>`. In other languages like C++ this is called a [multimap].
/// Multimaps are usually preferred over `HashMap<K, Vec<V>>` in cases where there are few
/// duplicates since the overhead of the Vec is unnecessary in most lookups.
///
/// In Spellbook this type is used to represent the "WordList". Hunspell-like dictionaries are
/// defined as sets of "stems" and a collection of "flags" that apply to that stem. Some
/// dictionaries provide multiple definitions of a stem with different sets of flags. Naively
/// merging these stems is not correct: the flags in one set might prevent an affix from
/// compounding while another set of flags provides a different affix which supports compounding.
///
/// Internally this is built on Hashbrown's "raw" API - a set of tools for building [Swiss
/// Tables].
///
/// [multimap]: https://en.cppreference.com/w/cpp/container/multimap
/// [Swiss Tables]: https://abseil.io/blog/20180927-swisstables
#[derive(Clone)]
pub struct HashBag<K, V, S> {
    table: HashTable<(K, V)>,
    build_hasher: S,
}

impl<K, V, S> HashBag<K, V, S> {
    /// Returns an iterator over the key-value pairs in the bag.
    ///
    /// The ordering of the pairs returned by the iterator is undefined.
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            inner: self.table.iter(),
        }
    }

    /// The number of key-value pairs in the table.
    pub fn len(&self) -> usize {
        self.table.len()
    }
}

impl<K, V, S> HashBag<K, V, S>
where
    K: Hash + Eq,
    S: BuildHasher,
{
    pub fn with_capacity_and_hasher(capacity: usize, build_hasher: S) -> Self {
        Self {
            table: HashTable::with_capacity(capacity),
            build_hasher,
        }
    }

    /// Inserts a key-value pair into the bag.
    ///
    /// Duplicate keys or entire key-value pairs are permitted.
    pub fn insert(&mut self, k: K, v: V) {
        let hash = make_hash(&self.build_hasher, &k);
        let hasher = make_hasher(&self.build_hasher);
        // Insert without attempting to find an existing entry with this key.
        self.table.insert_unique(hash, (k, v), hasher);
    }

    /// Gets all key-value pairs in the bag with the given key.
    // NOTE: we return the key strictly for lifetime reasons: we can "smuggle" owned Cows through
    // the bag.
    pub fn get<'bag, 'key, Q>(&'bag self, k: &'key Q) -> GetAllIter<'bag, 'key, Q, K, V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = make_hash(&self.build_hasher, k);

        GetAllIter {
            inner: self.table.iter_hash(hash),
            key: k,
        }
    }

    /// Gets all values for any pairs in the table with the given key.
    ///
    /// This is the same as `get` but only exclusive borrows of the values are returned.
    pub fn get_mut<'bag, 'key, Q>(&'bag mut self, k: &'key Q) -> GetAllMutIter<'bag, 'key, Q, K, V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = make_hash(&self.build_hasher, k);

        GetAllMutIter {
            inner: self.table.iter_hash_mut(hash),
            key: k,
        }
    }
}

impl<K, V, S> Debug for HashBag<K, V, S>
where
    K: Debug + Hash + Eq,
    V: Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

// `make_hash`, `make_hasher`, and `Iter` are pulled from Hashbrown's `map` module
// at `274c7bbd79398881e0225c0133e423ce60d7a8f1`.

fn make_hash<Q, S>(hash_builder: &S, val: &Q) -> u64
where
    Q: Hash + ?Sized,
    S: BuildHasher,
{
    use core::hash::Hasher;
    let mut state = hash_builder.build_hasher();
    val.hash(&mut state);
    state.finish()
}

fn make_hasher<Q, V, S>(hash_builder: &S) -> impl Fn(&(Q, V)) -> u64 + '_
where
    Q: Hash,
    S: BuildHasher,
{
    move |val| make_hash::<Q, S>(hash_builder, &val.0)
}

// This is a very thin wrapper around `hash_table::Iter` which rearranges the reference so that
// we return `(&k, &v)` instead of `&(k, v)`.
pub struct Iter<'a, K, V> {
    inner: hash_table::Iter<'a, (K, V)>,
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<(&'a K, &'a V)> {
        let (k, v) = self.inner.next()?;
        Some((k, v))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<K, V> ExactSizeIterator for Iter<'_, K, V> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

pub struct GetAllIter<'bag, 'key, Q: ?Sized, K, V>
where
    K: Borrow<Q>,
    Q: Hash + Eq,
{
    inner: hash_table::IterHash<'bag, (K, V)>,
    key: &'key Q,
}

impl<'bag, Q: ?Sized, K, V> Iterator for GetAllIter<'bag, '_, Q, K, V>
where
    K: Borrow<Q>,
    Q: Hash + Eq,
{
    type Item = (&'bag K, &'bag V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next() {
                Some((k, v)) => {
                    if self.key.eq(k.borrow()) {
                        return Some((k, v));
                    }
                    continue;
                }
                None => return None,
            }
        }
    }
}

pub struct GetAllMutIter<'bag, 'key, Q: ?Sized, K, V>
where
    K: Borrow<Q>,
    Q: Hash + Eq,
{
    inner: hash_table::IterHashMut<'bag, (K, V)>,
    key: &'key Q,
}

impl<'bag, Q: ?Sized, K, V> Iterator for GetAllMutIter<'bag, '_, Q, K, V>
where
    K: Borrow<Q>,
    Q: Hash + Eq,
{
    type Item = &'bag mut V;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next() {
                Some((k, v)) => {
                    if self.key.eq((*k).borrow()) {
                        return Some(v);
                    }
                    continue;
                }
                None => return None,
            }
        }
    }
}

#[cfg(test)]
mod test {
    use core::hash::BuildHasher;

    use crate::alloc::{string::ToString, vec::Vec};
    use crate::DefaultHashBuilder;

    impl<K, V, S: BuildHasher + Default> super::HashBag<K, V, S> {
        pub fn new() -> Self {
            Self {
                table: super::HashTable::new(),
                build_hasher: S::default(),
            }
        }
    }

    type HashBag<K, V> = super::HashBag<K, V, DefaultHashBuilder>;

    #[test]
    fn insert_and_get_duplicate_keys() {
        let mut bag = HashBag::new();
        bag.insert(1, 1);
        bag.insert(5, 5);
        assert!(bag.len() == 2);
        bag.insert(1, 1);
        bag.insert(1, 2);
        assert!(bag.len() == 4);

        let mut vals: Vec<_> = bag.get(&1).map(|kv| kv.1).copied().collect();
        vals.sort_unstable();
        assert_eq!(&[1, 1, 2], vals.as_slice());
        let vals = bag.get(&5).map(|kv| kv.1).copied().collect::<Vec<_>>();
        assert_eq!(&[5], vals.as_slice());
    }

    #[test]
    fn string_keys() {
        let mut bag = HashBag::new();
        bag.insert("hello".to_string(), "bob");
        bag.insert("hello".to_string(), "world");
        bag.insert("bye".to_string(), "bob");

        let mut hellos: Vec<_> = bag.get("hello").map(|kv| kv.1).copied().collect();
        hellos.sort_unstable();
        assert_eq!(&["bob", "world"], hellos.as_slice());

        let vals: Vec<_> = bag.get("bye").map(|kv| kv.1).copied().collect();
        assert_eq!(&["bob"], vals.as_slice());
    }

    #[test]
    fn lookup_correctness_on_large_corpus() {
        let max = 100_000;
        let expected: Vec<_> = (1..max).flat_map(|n| [(n, n), (n, n + 1)]).collect();

        let mut bag = HashBag::new();
        for (k, v) in expected.iter() {
            bag.insert(*k, *v);
        }

        let mut buf = Vec::with_capacity(2);
        for n in 1..max {
            buf.clear();
            buf.extend(bag.get(&n).map(|(k, v)| (*k, *v)));
            buf.sort_unstable();
            assert_eq!(&[(n, n), (n, n + 1)], buf.as_slice());
        }
    }

    #[test]
    fn iter() {
        // The iterator is currently unused but very small and could be useful for debugging.
        let pairs = [(1, 1), (1, 2), (1, 3), (3, 1)];
        let mut bag = HashBag::new();
        for (k, v) in pairs {
            bag.insert(k, v);
        }

        assert_eq!(bag.iter().len(), pairs.len());

        let mut values: Vec<_> = bag.iter().map(|(k, v)| (*k, *v)).collect();
        values.sort_unstable();
        assert_eq!(values, pairs);
    }

    #[test]
    fn display() {
        // Shameless coverage test, it brings the file to 100% :P
        let pairs = &[(1, 1), (1, 1), (1, 2), (1, 3), (3, 1)];
        let mut bag = super::HashBag::with_capacity_and_hasher(
            pairs.len(),
            // We use a hard-coded seed so that the display is deterministic.
            foldhash::fast::FixedState::with_seed(1234567810),
        );
        for (k, v) in pairs {
            bag.insert(k, v);
        }

        assert_eq!(
            "{1: 1, 1: 1, 1: 2, 1: 3, 3: 1}",
            crate::alloc::format!("{bag:?}").as_str()
        );
    }

    #[test]
    fn mutate() {
        let pairs = [(1, 1), (1, 2), (1, 3), (3, 1)];
        let mut bag = HashBag::new();
        for (k, v) in pairs {
            bag.insert(k, v);
        }

        for mut_v in bag.get_mut(&1) {
            *mut_v *= 2;
        }

        let mut values: Vec<_> = bag.iter().map(|(k, v)| (*k, *v)).collect();
        values.sort_unstable();
        assert_eq!(values, [(1, 2), (1, 4), (1, 6), (3, 1)]);
    }
}
