/*!
[![](https://docs.rs/generational-arena/badge.svg)](https://docs.rs/generational-arena/)
[![](https://img.shields.io/crates/v/generational-arena.svg)](https://crates.io/crates/generational-arena)
[![](https://img.shields.io/crates/d/generational-arena.svg)](https://crates.io/crates/generational-arena)
[![Travis CI Build Status](https://travis-ci.org/fitzgen/generational-arena.svg?branch=master)](https://travis-ci.org/fitzgen/generational-arena)

A safe arena allocator that allows deletion without suffering from [the ABA
problem](https://en.wikipedia.org/wiki/ABA_problem) by using generational
indices.

Inspired by [Catherine West's closing keynote at RustConf
2018](http://rustconf.com/program.html#closingkeynote), where these ideas
were presented in the context of an Entity-Component-System for games
programming.

## What? Why?

Imagine you are working with a graph and you want to add and delete individual
nodes at a time, or you are writing a game and its world consists of many
inter-referencing objects with dynamic lifetimes that depend on user
input. These are situations where matching Rust's ownership and lifetime rules
can get tricky.

It doesn't make sense to use shared ownership with interior mutability (ie
`Rc<RefCell<T>>` or `Arc<Mutex<T>>`) nor borrowed references (ie `&'a T` or `&'a
mut T`) for structures. The cycles rule out reference counted types, and the
required shared mutability rules out borrows. Furthermore, lifetimes are dynamic
and don't follow the borrowed-data-outlives-the-borrower discipline.

In these situations, it is tempting to store objects in a `Vec<T>` and have them
reference each other via their indices. No more borrow checker or ownership
problems! Often, this solution is good enough.

However, now we can't delete individual items from that `Vec<T>` when we no
longer need them, because we end up either

* messing up the indices of every element that follows the deleted one, or

* suffering from the [ABA
  problem](https://en.wikipedia.org/wiki/ABA_problem). To elaborate further, if
  we tried to replace the `Vec<T>` with a `Vec<Option<T>>`, and delete an
  element by setting it to `None`, then we create the possibility for this buggy
  sequence:

    * `obj1` references `obj2` at index `i`

    * someone else deletes `obj2` from index `i`, setting that element to `None`

    * a third thing allocates `obj3`, which ends up at index `i`, because the
      element at that index is `None` and therefore available for allocation

    * `obj1` attempts to get `obj2` at index `i`, but incorrectly is given
      `obj3`, when instead the get should fail.

By introducing a monotonically increasing generation counter to the collection,
associating each element in the collection with the generation when it was
inserted, and getting elements from the collection with the *pair* of index and
the generation at the time when the element was inserted, then we can solve the
aforementioned ABA problem. When indexing into the collection, if the index
pair's generation does not match the generation of the element at that index,
then the operation fails.

## Features

* Zero `unsafe`
* Well tested, including quickchecks
* `no_std` compatibility
* All the trait implementations you expect: `IntoIterator`, `FromIterator`,
  `Extend`, etc...

## Usage

First, add `generational-arena` to your `Cargo.toml`:

```toml
[dependencies]
generational-arena = "0.1"
```

Then, import the crate and use the
[`generational_arena::Arena`](./struct.Arena.html) type!

```rust
extern crate generational_arena;
use generational_arena::Arena;

let mut arena = Arena::new();

// Insert some elements into the arena.
let rza = arena.insert("Robert Fitzgerald Diggs");
let gza = arena.insert("Gary Grice");
let bill = arena.insert("Bill Gates");

// Inserted elements can be accessed infallibly via indexing (and missing
// entries will panic).
assert_eq!(arena[rza], "Robert Fitzgerald Diggs");

// Alternatively, the `get` and `get_mut` methods provide fallible lookup.
if let Some(genius) = arena.get(gza) {
    println!("The gza gza genius: {}", genius);
}
if let Some(val) = arena.get_mut(bill) {
    *val = "Bill Gates doesn't belong in this set...";
}

// We can remove elements.
arena.remove(bill);

// Insert a new one.
let murray = arena.insert("Bill Murray");

// The arena does not contain `bill` anymore, but it does contain `murray`, even
// though they are almost certainly at the same index within the arena in
// practice. Ambiguities are resolved with an associated generation tag.
assert!(!arena.contains(bill));
assert!(arena.contains(murray));

// Iterate over everything inside the arena.
for (idx, value) in &arena {
    println!("{:?} is at {:?}", value, idx);
}
```

## `no_std`

To enable `no_std` compatibility, disable the on-by-default "std" feature. This
currently requires nightly Rust and `feature(alloc)` to get access to `Vec`.

```toml
[dependencies]
generational-arena = { version = "0.1", default-features = false }
```

### Serialization/deserialization

To enable serialization/deserialization support, enable the "serde" feature.

```toml
[dependencies]
generational-arena = { version = "0.1", features = ["serde"] }
```
 */

#![forbid(unsafe_code, missing_docs, missing_debug_implementations)]
#![no_std]
#![cfg_attr(not(feature = "std"), feature(alloc))]

#[macro_use]
extern crate cfg_if;
#[cfg(feature = "serde")]
extern crate serde;

cfg_if! {
    if #[cfg(feature = "std")] {
        extern crate std;
        use std::vec::{self, Vec};
    } else {
        extern crate alloc;
        use alloc::vec::{self, Vec};
    }
}

use core::cmp;
use core::iter::{self, Extend, FromIterator};
use core::mem;
use core::ops;
use core::slice;

#[cfg(feature = "serde")]
mod serde_impl;

/// The `Arena` allows inserting and removing elements that are referred to by
/// `Index`.
///
/// [See the module-level documentation for example usage and motivation.](./index.html)
#[derive(Clone, Debug)]
pub struct Arena<T> {
    items: Vec<Entry<T>>,
    generation: u64,
    free_list_head: Option<usize>,
}

#[derive(Clone, Debug)]
enum Entry<T> {
    Free { next_free: Option<usize> },
    Occupied { generation: u64, value: T },
}

/// An index (and generation) into an `Arena`.
///
/// To get an `Index`, insert an element into an `Arena`, and the `Index` for
/// that element will be returned.
///
/// # Examples
///
/// ```
/// use generational_arena::Arena;
///
/// let mut arena = Arena::new();
/// let idx = arena.insert(123);
/// assert_eq!(arena[idx], 123);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Index {
    index: usize,
    generation: u64,
}

const DEFAULT_CAPACITY: usize = 4;

impl<T> Arena<T> {
    /// Constructs a new, empty `Arena`.
    ///
    /// # Examples
    ///
    /// ```
    /// use generational_arena::Arena;
    ///
    /// let mut arena = Arena::<usize>::new();
    /// # let _ = arena;
    /// ```
    pub fn new() -> Arena<T> {
        Arena::with_capacity(DEFAULT_CAPACITY)
    }

    /// Constructs a new, empty `Arena<T>` with the specified capacity.
    ///
    /// The `Arena<T>` will be able to hold `n` elements without further allocation.
    ///
    /// # Examples
    ///
    /// ```
    /// use generational_arena::Arena;
    ///
    /// let mut arena = Arena::with_capacity(10);
    ///
    /// // These insertions will not require further allocation.
    /// for i in 0..10 {
    ///     assert!(arena.try_insert(i).is_ok());
    /// }
    ///
    /// // But now we are at capacity, and there is no more room.
    /// assert!(arena.try_insert(99).is_err());
    /// ```
    pub fn with_capacity(n: usize) -> Arena<T> {
        let n = cmp::max(n, 1);
        let mut arena = Arena {
            items: Vec::new(),
            generation: 0,
            free_list_head: None,
        };
        arena.reserve(n);
        arena
    }

    /// Attempts to insert `value` into the arena using existing capacity.
    ///
    /// This method will never allocate new capacity in the arena.
    ///
    /// If insertion succeeds, then the `value`'s index is returned. If
    /// insertion fails, then `Err(value)` is returned to give ownership of
    /// `value` back to the caller.
    ///
    /// # Examples
    ///
    /// ```
    /// use generational_arena::Arena;
    ///
    /// let mut arena = Arena::new();
    ///
    /// match arena.try_insert(42) {
    ///     Ok(idx) => {
    ///         // Insertion succeeded.
    ///         assert_eq!(arena[idx], 42);
    ///     }
    ///     Err(x) => {
    ///         // Insertion failed.
    ///         assert_eq!(x, 42);
    ///     }
    /// };
    /// ```
    #[inline]
    pub fn try_insert(&mut self, value: T) -> Result<Index, T> {
        match self.free_list_head {
            None => Err(value),
            Some(i) => match self.items[i] {
                Entry::Occupied { .. } => panic!("corrupt free list"),
                Entry::Free { next_free } => {
                    self.free_list_head = next_free;
                    self.items[i] = Entry::Occupied {
                        generation: self.generation,
                        value,
                    };
                    Ok(Index {
                        index: i,
                        generation: self.generation,
                    })
                }
            },
        }
    }

    /// Insert `value` into the arena, allocating more capacity if necessary.
    ///
    /// The `value`'s associated index in the arena is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use generational_arena::Arena;
    ///
    /// let mut arena = Arena::new();
    ///
    /// let idx = arena.insert(42);
    /// assert_eq!(arena[idx], 42);
    /// ```
    #[inline]
    pub fn insert(&mut self, value: T) -> Index {
        match self.try_insert(value) {
            Ok(i) => i,
            Err(value) => {
                self.insert_slow_path(value)
            }
        }
    }

    #[inline(never)]
    fn insert_slow_path(&mut self, value: T) -> Index {
        let len = self.items.len();
        self.reserve(len);
        self.try_insert(value)
            .map_err(|_| ())
            .expect("inserting will always succeed after reserving additional space")

    }

    /// Remove the element at index `i` from the arena.
    ///
    /// If the element at index `i` is still in the arena, then it is
    /// returned. If it is not in the arena, then `None` is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use generational_arena::Arena;
    ///
    /// let mut arena = Arena::new();
    /// let idx = arena.insert(42);
    ///
    /// assert_eq!(arena.remove(idx), Some(42));
    /// assert_eq!(arena.remove(idx), None);
    /// ```
    pub fn remove(&mut self, i: Index) -> Option<T> {
        if i.index >= self.items.len() {
            return None;
        }

        let entry = mem::replace(
            &mut self.items[i.index],
            Entry::Free {
                next_free: self.free_list_head,
            },
        );
        match entry {
            Entry::Occupied { generation, value } => if generation == i.generation {
                self.generation += 1;
                self.free_list_head = Some(i.index);
                Some(value)
            } else {
                self.items[i.index] = Entry::Occupied { generation, value };
                None
            },
            e @ Entry::Free { .. } => {
                self.items[i.index] = e;
                None
            }
        }
    }

    /// Is the element at index `i` in the arena?
    ///
    /// Returns `true` if the element at `i` is in the arena, `false` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use generational_arena::Arena;
    ///
    /// let mut arena = Arena::new();
    /// let idx = arena.insert(42);
    ///
    /// assert!(arena.contains(idx));
    /// arena.remove(idx);
    /// assert!(!arena.contains(idx));
    /// ```
    pub fn contains(&self, i: Index) -> bool {
        self.get(i).is_some()
    }

    /// Get a shared reference to the element at index `i` if it is in the
    /// arena.
    ///
    /// If the element at index `i` is not in the arena, then `None` is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use generational_arena::Arena;
    ///
    /// let mut arena = Arena::new();
    /// let idx = arena.insert(42);
    ///
    /// assert_eq!(arena.get(idx), Some(&42));
    /// arena.remove(idx);
    /// assert!(arena.get(idx).is_none());
    /// ```
    pub fn get(&self, i: Index) -> Option<&T> {
        match self.items.get(i.index) {
            Some(Entry::Occupied {
                generation,
                ref value,
            })
                if *generation == i.generation =>
            {
                Some(value)
            }
            _ => None,
        }
    }

    /// Get an exclusive reference to the element at index `i` if it is in the
    /// arena.
    ///
    /// If the element at index `i` is not in the arena, then `None` is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use generational_arena::Arena;
    ///
    /// let mut arena = Arena::new();
    /// let idx = arena.insert(42);
    ///
    /// *arena.get_mut(idx).unwrap() += 1;
    /// assert_eq!(arena.remove(idx), Some(43));
    /// assert!(arena.get_mut(idx).is_none());
    /// ```
    pub fn get_mut(&mut self, i: Index) -> Option<&mut T> {
        match self.items.get_mut(i.index) {
            Some(Entry::Occupied {
                generation,
                ref mut value,
            })
                if *generation == i.generation =>
            {
                Some(value)
            }
            _ => None,
        }
    }

    /// Get the capacity of this arena.
    ///
    /// The capacity is the maximum number of elements the arena can hold
    /// without further allocation, including however many it currently
    /// contains.
    ///
    /// # Examples
    ///
    /// ```
    /// use generational_arena::Arena;
    ///
    /// let mut arena = Arena::with_capacity(10);
    /// assert_eq!(arena.capacity(), 10);
    ///
    /// // `try_insert` does not allocate new capacity.
    /// for i in 0..10 {
    ///     assert!(arena.try_insert(1).is_ok());
    ///     assert_eq!(arena.capacity(), 10);
    /// }
    ///
    /// // But `insert` will if the arena is already at capacity.
    /// arena.insert(0);
    /// assert!(arena.capacity() > 10);
    /// ```
    pub fn capacity(&self) -> usize {
        self.items.len()
    }

    /// Allocate space for `additional_capacity` more elements in the arena.
    ///
    /// # Panics
    ///
    /// Panics if this causes the capacity to overflow.
    ///
    /// # Examples
    ///
    /// ```
    /// use generational_arena::Arena;
    ///
    /// let mut arena = Arena::with_capacity(10);
    /// arena.reserve(5);
    /// assert_eq!(arena.capacity(), 15);
    /// # let _: Arena<usize> = arena;
    /// ```
    pub fn reserve(&mut self, additional_capacity: usize) {
        let start = self.items.len();
        let end = self.items.len() + additional_capacity;
        let old_head = self.free_list_head;
        self.items.reserve_exact(additional_capacity);
        self.items.extend((start..end).map(|i| {
            if i == end - 1 {
                Entry::Free {
                    next_free: old_head,
                }
            } else {
                Entry::Free {
                    next_free: Some(i + 1),
                }
            }
        }));
        self.free_list_head = Some(start);
    }

    /// Iterate over shared references to the elements in this arena.
    ///
    /// Yields pairs of `(Index, &T)` items.
    ///
    /// Order of iteration is not defined.
    ///
    /// # Examples
    ///
    /// ```
    /// use generational_arena::Arena;
    ///
    /// let mut arena = Arena::new();
    /// for i in 0..10 {
    ///     arena.insert(i * i);
    /// }
    ///
    /// for (idx, value) in arena.iter() {
    ///     println!("{} is at index {:?}", value, idx);
    /// }
    /// ```
    pub fn iter(&self) -> Iter<T> {
        Iter {
            inner: self.items.iter().enumerate()
        }
    }

    /// Iterate over exclusive references to the elements in this arena.
    ///
    /// Yields pairs of `(Index, &mut T)` items.
    ///
    /// Order of iteration is not defined.
    ///
    /// # Examples
    ///
    /// ```
    /// use generational_arena::Arena;
    ///
    /// let mut arena = Arena::new();
    /// for i in 0..10 {
    ///     arena.insert(i * i);
    /// }
    ///
    /// for (_idx, value) in arena.iter_mut() {
    ///     *value += 5;
    /// }
    /// ```
    pub fn iter_mut(&mut self) -> IterMut<T> {
        IterMut {
            inner: self.items.iter_mut().enumerate()
        }
    }
}

impl<T> IntoIterator for Arena<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        IntoIter {
            inner: self.items.into_iter()
        }
    }
}

/// An iterator over the elements in an arena.
///
/// Yields `T` items.
///
/// Order of iteration is not defined.
///
/// # Examples
///
/// ```
/// use generational_arena::Arena;
///
/// let mut arena = Arena::new();
/// for i in 0..10 {
///     arena.insert(i * i);
/// }
///
/// for value in arena {
///     assert!(value < 100);
/// }
/// ```
#[derive(Clone, Debug)]
pub struct IntoIter<T> {
    inner: vec::IntoIter<Entry<T>>,
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next() {
                Some(Entry::Free { .. }) => continue,
                Some(Entry::Occupied { value, .. }) => return Some(value),
                None => return None,
            }
        }
    }
}

impl<'a, T> IntoIterator for &'a Arena<T> {
    type Item = (Index, &'a T);
    type IntoIter = Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// An iterator over shared references to the elements in an arena.
///
/// Yields pairs of `(Index, &T)` items.
///
/// Order of iteration is not defined.
///
/// # Examples
///
/// ```
/// use generational_arena::Arena;
///
/// let mut arena = Arena::new();
/// for i in 0..10 {
///     arena.insert(i * i);
/// }
///
/// for (idx, value) in &arena {
///     println!("{} is at index {:?}", value, idx);
/// }
/// ```
#[derive(Clone, Debug)]
pub struct Iter<'a, T: 'a> {
    inner: iter::Enumerate<slice::Iter<'a, Entry<T>>>,
}

impl<'a, T> Iterator for Iter<'a, T> {
    type Item = (Index, &'a T);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next() {
                Some((_, &Entry::Free { .. })) => continue,
                Some((index, &Entry::Occupied { generation, ref value })) => {
                    let idx = Index { index, generation };
                    return Some((idx, value))
                },
                None => return None,
            }
        }
    }
}

impl<'a, T> IntoIterator for &'a mut Arena<T> {
    type Item = (Index, &'a mut T);
    type IntoIter = IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

/// An iterate over exclusive references to elements in this arena.
///
/// Yields pairs of `(Index, &mut T)` items.
///
/// Order of iteration is not defined.
///
/// # Examples
///
/// ```
/// use generational_arena::Arena;
///
/// let mut arena = Arena::new();
/// for i in 0..10 {
///     arena.insert(i * i);
/// }
///
/// for (_idx, value) in &mut arena {
///     *value += 5;
/// }
/// ```
#[derive(Debug)]
pub struct IterMut<'a, T: 'a> {
    inner: iter::Enumerate<slice::IterMut<'a, Entry<T>>>,
}

impl<'a, T> Iterator for IterMut<'a, T> {
    type Item = (Index, &'a mut T);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next() {
                Some((_, &mut Entry::Free { .. })) => continue,
                Some((index, &mut Entry::Occupied { generation, ref mut value })) => {
                    let idx = Index { index, generation };
                    return Some((idx, value))
                },
                None => return None,
            }
        }
    }
}

impl<T> Extend<T> for Arena<T> {
    fn extend<I: IntoIterator<Item=T>>(&mut self, iter: I) {
        for t in iter {
            self.insert(t);
        }
    }
}

impl<T> FromIterator<T> for Arena<T> {
    fn from_iter<I: IntoIterator<Item=T>>(iter: I) -> Self {
        let iter = iter.into_iter();
        let (lower, upper) = iter.size_hint();
        let cap = upper.unwrap_or(lower);
        let cap = cmp::max(cap, 1);
        let mut arena = Arena::with_capacity(cap);
        arena.extend(iter);
        arena
    }
}

impl<T> ops::Index<Index> for Arena<T> {
    type Output = T;

    fn index(&self, index: Index) -> &Self::Output {
        self.get(index).unwrap()
    }
}

impl<T> ops::IndexMut<Index> for Arena<T> {
    fn index_mut(&mut self, index: Index) -> &mut Self::Output {
        self.get_mut(index).unwrap()
    }
}
