// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later
//! [`OrderedTable`] provides a simple interface for storing data with guaranteed uniqueness,
//! persistence management, entry order manipulation, and sorting.
//! Implement [`TableEntry`] for the data type you want to store, then create an [`OrderedTable`]
//! with a [`FilePersistence`] handler, or a [`Persistence`] handler of your own.
//! ```ignore
//! use std::string::String;
//!
//! use ordered_table::{FilePersistence, OrderedTable, OrderedTableError, TableEntry};
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize, Clone)]
//! struct MyString(String);
//!
//! // This satisfies the `Serialize` trait bounds
//! impl ToString for MyString {
//!     fn to_string(&self) -> String { self.0.clone() }
//! }
//!
//! impl TableEntry for MyString {
//!     type DuplicateReason = ();
//!     type ValidationError = ();
//!
//!     fn is_duplicate(&self, other: &Self) -> Option<Self::DuplicateReason> {
//!         if self.0 == other.0 {
//!             return Some(());
//!         }
//!         None
//!     }
//!
//!     fn validate(&self) -> Result<(), Self::ValidationError> {
//!         if self.0.is_empty() {
//!             return Err(());
//!         }
//!         Ok(())
//!     }
//! }
//!
//! fn my_table() -> Result<(), OrderedTableError<MyString>> {
//!     let table = OrderedTable::<MyString, FilePersistence<fs_permissions::FileSystemPermissions>>::new().with_persistence(
//!         FilePersistence::new(String::from("my_string_table"), fs::Location::AppData),
//!     )?;
//!     Ok(())
//! }
//! ```

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum OrderedTableError<T: TableEntry> {
    #[error("Persistence error: {0:?}")]
    PersistenceError(PersistenceError),
    #[error(
        "Attempted to load a duplicate, entry {0:?} matches {1:?}, make sure is_duplicate has not changed."
    )]
    LoadDuplicateError(usize, DuplicateInfo<T>),
    #[error("Attempted to load an invalid entry {0:?}: {1:?}, make sure validate has not changed.")]
    LoadInvalidError(usize, T::ValidationError),
    #[error("Index of {0:?} out of bounds, table length is {0:?}")]
    OutOfBoundsError(usize, usize),
    #[error(
        "TableEntry is_duplicate function is non-reflexive at index {0:?}: left_self: {1:?}, right_self: {2:?}"
    )]
    NonReflexiveError(Option<usize>, Option<T::DuplicateReason>, Option<T::DuplicateReason>),
    #[error("TableEntry is_duplicate function is non-deterministic at index {0:?}: left_self: {1:?}, right_self: {2:?}, left_match: {3:?}, right_match: {4:?}")]
    NonDeterministicError(
        Option<usize>,
        Option<T::DuplicateReason>,
        Option<T::DuplicateReason>,
        Option<T::DuplicateReason>,
        Option<T::DuplicateReason>,
    ),
    #[error("Attempted to push an invalid entry: {0:?}")]
    PushInvalidError(T::ValidationError),
    #[error("Attempted to push a duplicate entry: {0:?}")]
    PushDuplicateError(DuplicateInfo<T>),
    #[error("Attempted an invalid edit operation on an entry: {0:?}")]
    EditInvalidOperationError(T::ValidationError),
    #[error("Attempted an edit that created an invalid entry: {0:?}")]
    EditInvalidResultError(T::ValidationError),
    #[error("Attempted an edit that created a duplicate entry: {0:?}")]
    EditDuplicateError(DuplicateInfo<T>),
    #[error("Could not move item from {0:?} by {1:?} positions, would cause an overflow or underflow")]
    MoveOverflowError(usize, isize),
    #[error("Could not handle category operations, categories are not separated")]
    CategoryError,
    #[error("Could not move item from {0:?} in category {1:?} to {2:?} in category {3:?},")]
    CategoryOutOfBoundsError(usize, u32, usize, u32),
}

impl<T: TableEntry> From<PersistenceError> for OrderedTableError<T> {
    fn from(value: PersistenceError) -> Self { Self::PersistenceError(value) }
}

/// Implement this trait on the type you want to store in an [`OrderedTable`].
/// It requires traits [`serde::Serialize`], [`serde::Deserialize`], and [`std::clone::Clone`].
pub trait TableEntry: Serialize + for<'de> Deserialize<'de> + Clone {
    /// Use an enum to describe why two keys match,
    /// and store any data necessary for relevant messaging.
    /// If you are sure that only one match type is possible,
    /// feel free to use `()` or a single type.
    ///
    /// Avoid putting sensitive information in this, as it is likely
    /// that you will eventually print it to a debug log.
    type DuplicateReason: std::fmt::Debug;

    /// Use an enum to describe why an entry or edit operaion
    /// is invalid, and store any data necessary for relevant messaging.
    /// If you are sure that only one validation error is possible,
    /// feel free to use `()` or a single type.
    ///
    /// Avoid putting sensitive information in this, as it is likely
    /// that you will eventually print it to a debug log.
    type ValidationError: std::fmt::Debug;

    /// Implement this such that entries with matching keys return
    /// `Some(DuplicateReason)`, and non-matching keys return None.
    ///
    /// To disable uniqueness enforcement, always return None.
    ///
    /// Ensure this has the reflexive property `(a.is_duplicate(&a) == Some(DuplicateReason))`.
    /// An `OrderedTableError::NonReflexiveError` will be returned if `OrderedTable` detects a violation.
    fn is_duplicate(&self, other: &Self) -> Option<Self::DuplicateReason>;

    /// This adds some convenience to pushing and editing items,
    /// so validation happens automatically.
    ///
    /// If you want validation to be done, implement this such that valid
    /// entries return `Ok(())` and invalid entries return `Err(Self::ValidationError)`.
    fn validate(&self) -> Result<(), Self::ValidationError> { Ok(()) }
}

/// This provides your app's [`TableEntry::DuplicateReason`], as well as the index of the entry that
/// matched the entry you searched for with `find` or tried to `push`.
pub type DuplicateInfo<T> = (<T as TableEntry>::DuplicateReason, usize);

#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("Failed to load: {0:?}")]
    LoadError(String),
    #[error("Failed to save: {0:?}")]
    SaveError(String),
}

/// This allows you to define a custom persistence handler.
pub trait Persistence {
    /// Load a vector [`TableEntry`] items from your persistence handler to a deserializable string.
    fn load(&mut self) -> Result<String, PersistenceError>;

    /// Save a string of serialized [`TableEntry`] items to your persistence handler.
    fn save(&mut self, table: &String) -> Result<(), PersistenceError>;
}

pub struct NoPersistence;

impl Persistence for NoPersistence {
    fn load(&mut self) -> Result<String, PersistenceError> {
        Err(PersistenceError::LoadError(String::from("No persistence type specified")))
    }

    fn save(&mut self, _table: &String) -> Result<(), PersistenceError> {
        Err(PersistenceError::SaveError(String::from("No persistence type specified")))
    }
}

/// This provides a simple way to keep your [`OrderedTable`] stored in a file.
pub struct FilePersistence<P: server::CheckedPermissions> {
    path: String,
    location: fs::Location,
    _permission: core::marker::PhantomData<fn() -> P>,
}

impl<P: server::CheckedPermissions> FilePersistence<P> {
    pub fn new(path: String, location: fs::Location) -> Self {
        // A save failure panics, so the table may only live somewhere that stays mounted
        // for as long as the app does.
        assert!(
            !matches!(location, fs::Location::Usb | fs::Location::Airlock),
            "FilePersistence: {location:?} can go away at runtime"
        );
        Self { path, location, _permission: Default::default() }
    }
}

impl<P> Persistence for FilePersistence<P>
where
    P: server::CheckedPermissions + fs::DurableFilePermissions,
{
    fn load(&mut self) -> Result<String, PersistenceError> {
        let file_system = fs::FileSystem::<P>::default();
        let table = match file_system.durable_file_read(&self.path, self.location) {
            Ok(table) => table,
            // The file only appears on the first save, so an absent one is an empty table.
            Err(fs::Error::FileNotFound) => return Ok(String::new()),
            Err(e) => return Err(PersistenceError::LoadError(e.to_string())),
        };

        String::from_utf8(table).map_err(|e| PersistenceError::LoadError(e.to_string()))
    }

    fn save(&mut self, table: &String) -> Result<(), PersistenceError> {
        let file_system = fs::FileSystem::<P>::default();
        file_system
            .durable_file_write(&self.path, self.location, table.as_bytes())
            .map_err(|e| PersistenceError::SaveError(e.to_string()))
    }
}

pub struct OrderedTable<T: TableEntry, P: Persistence = NoPersistence> {
    // Does this need to be pub(crate) to enable edit_where?
    pub(crate) table: Vec<T>,
    persistence: Option<P>,
}

impl<T: TableEntry, P: Persistence> OrderedTable<T, P> {
    pub fn new() -> Self { Self { table: Vec::new(), persistence: None } }

    fn check_reflexive(
        &self,
        index: Option<usize>,
        entry: &T,
    ) -> Result<Option<T::DuplicateReason>, OrderedTableError<T>> {
        let match_self = entry.is_duplicate(entry);

        if match_self.is_none() {
            return Err(OrderedTableError::NonReflexiveError(index, None, None));
        }

        Ok(match_self)
    }

    /// This adds a [`Persistence`] handler to your [`OrderedTable`], and loads any persistent entries.
    pub fn with_persistence(mut self, mut persistence: P) -> Result<Self, OrderedTableError<T>> {
        let table_string = persistence.load()?;

        self.table = if table_string.is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(table_string.as_str())
                .map_err(|e| PersistenceError::LoadError(e.to_string()))?
        };

        for (i, e) in self.table.iter().enumerate() {
            self.check_reflexive(Some(i), &e)?;
            e.validate().map_err(|e| OrderedTableError::LoadInvalidError(i, e))?;

            if let Some(e) = self.find_exclude_index(&e, Some(i))? {
                return Err(OrderedTableError::LoadDuplicateError(i, e));
            }
        }

        self.persistence = Some(persistence);
        Ok(self)
    }

    /// Persist the table, panicking on failure.
    ///
    /// Construction already loaded through the same handler and nothing else writes the
    /// table behind our back, so a save that fails is a bug or a dead filesystem, not a
    /// fault a caller could do anything about. Dying here also stops the mutated in-memory
    /// table from outliving the contents it failed to reach.
    fn save(&mut self) {
        let Some(persistence) = &mut self.persistence else {
            return;
        };

        let table_string =
            serde_json::to_string(&self.table).expect("OrderedTable: entries failed to serialize");
        if let Err(e) = persistence.save(&table_string) {
            panic!("OrderedTable: failed to persist the table: {e}");
        }
    }

    fn find_exclude_index(
        &self,
        entry: &T,
        index: Option<usize>,
    ) -> Result<Option<DuplicateInfo<T>>, OrderedTableError<T>> {
        let left_self = self.check_reflexive(None, &entry)?;

        for (i, e) in self.table.iter().enumerate() {
            if index.is_some_and(|idx| i == idx) {
                continue;
            }

            // Prevent users from accidentally breaking uniqueness with a non-reflexive is_duplicate function
            let right_self = e.is_duplicate(&e);
            let left_match = entry.is_duplicate(&e);
            let right_match = e.is_duplicate(&entry);

            if right_self.is_none() || left_match.is_none() != right_match.is_none() {
                return Err(OrderedTableError::NonDeterministicError(
                    Some(i),
                    left_self,
                    right_self,
                    left_match,
                    right_match,
                ));
            }

            if let Some(reason) = left_match {
                return Ok(Some((reason, i)));
            }
        }

        Ok(None)
    }

    /// Find any entries that match your entry using [`TableEntry::is_duplicate`].
    pub fn find(&self, entry: &T) -> Result<Option<DuplicateInfo<T>>, OrderedTableError<T>> {
        self.find_exclude_index(entry, None)
    }

    /// Check that the entry can be pushed. This is a convenient way to see
    /// if your entry is valid and if there are any existing duplicates using [`TableEntry::is_duplicate`].
    pub fn validate_push(&self, entry: &T) -> Result<(), OrderedTableError<T>> {
        if let Some(e) = self.find(entry)? {
            return Err(OrderedTableError::PushDuplicateError(e));
        }

        entry.validate().map_err(|e| OrderedTableError::PushInvalidError(e))
    }

    /// Push an entry to the table. This fails if [`TableEntry::validate`] fails
    /// or if a duplicate is found with [`TableEntry::is_duplicate`].
    pub fn push(&mut self, entry: T) -> Result<(), OrderedTableError<T>> {
        self.validate_push(&entry)?;
        self.table.push(entry);
        self.save();
        Ok(())
    }

    fn check_insert_bounds(&self, index: usize) -> Result<(), OrderedTableError<T>> {
        let len = self.table.len();
        if index > len {
            return Err(OrderedTableError::OutOfBoundsError(index, len));
        }

        Ok(())
    }

    /// Insert an entry in the table. This fails if [`TableEntry::validate`] fails
    /// or if a duplicate is found with [`TableEntry::is_duplicate`].
    pub fn insert(&mut self, index: usize, entry: T) -> Result<(), OrderedTableError<T>> {
        self.check_insert_bounds(index)?;

        self.validate_push(&entry)?;
        self.table.insert(index, entry);
        self.save();
        Ok(())
    }

    fn check_bounds(&self, index: usize) -> Result<(), OrderedTableError<T>> {
        let len = self.table.len();
        if index >= len {
            return Err(OrderedTableError::OutOfBoundsError(index, len));
        }

        Ok(())
    }

    pub fn remove(&mut self, index: usize) -> Result<T, OrderedTableError<T>> {
        self.check_bounds(index)?;

        let entry = self.table.remove(index);
        self.save();
        Ok(entry)
    }

    pub fn get(&self, index: usize) -> Result<&T, OrderedTableError<T>> {
        self.check_bounds(index)?;
        Ok(&self.table[index])
    }

    /// Validate an edit to an item. This is a convenient way to check if your
    /// edit will produce an invalid or duplicate entry using [`TableEntry::validate`] or
    /// [`TableEntry::is_duplicate`].
    pub fn validate_edit<F>(&self, index: usize, mut edit_fn: F) -> Result<T, OrderedTableError<T>>
    where
        F: FnMut(&mut T) -> Result<(), T::ValidationError>,
    {
        self.check_bounds(index)?;

        let mut temp_entry = self.table[index].clone();
        edit_fn(&mut temp_entry).map_err(|e| OrderedTableError::EditInvalidOperationError(e))?;
        temp_entry.validate().map_err(|e| OrderedTableError::EditInvalidResultError(e))?;

        if let Some(e) = self.find_exclude_index(&temp_entry, Some(index))? {
            return Err(OrderedTableError::EditDuplicateError(e));
        }

        Ok(temp_entry)
    }

    /// Edit an item. This will fail if the provided edit produces an
    /// invalid or duplicate entry using [`TableEntry::validate`] or [`TableEntry::is_duplicate`].
    pub fn edit<F>(&mut self, index: usize, edit_fn: F) -> Result<(), OrderedTableError<T>>
    where
        F: FnMut(&mut T) -> Result<(), T::ValidationError>,
    {
        self.table[index] = self.validate_edit(index, edit_fn)?;
        self.save();
        Ok(())
    }

    /// This function validates all edits before implementing them, so it acts like an atomic database
    /// update. It ensures edits are validated in the context of other edits, so duplicates aren't made in
    /// the process.
    pub fn edit_where<F, W>(
        &mut self,
        mut edit_fn: F,
        mut where_fn: W,
    ) -> Result<(), Vec<(usize, OrderedTableError<T>)>>
    where
        F: FnMut(&mut T) -> Result<(), T::ValidationError>,
        W: FnMut(&T) -> bool,
    {
        let mut errors = Vec::<(usize, OrderedTableError<T>)>::new();
        let mut updated = Self { table: self.table.clone(), persistence: None };
        for (i, _) in self.table.iter().enumerate().filter(|(_, e)| where_fn(e)) {
            match updated.validate_edit(i, &mut edit_fn) {
                Ok(entry) => updated.table[i] = entry,
                Err(e) => errors.push((i, e)),
            }
        }

        if !errors.is_empty() {
            return Err(errors);
        }

        self.table = updated.table.clone();
        self.save();
        Ok(())
    }

    pub fn move_position(&mut self, index: usize, destination: usize) -> Result<(), OrderedTableError<T>> {
        if index == destination {
            return Ok(());
        }

        self.check_bounds(index)?;
        self.check_bounds(destination)?;

        let entry = self.table.remove(index);
        self.table.insert(destination, entry);
        self.save();
        Ok(())
    }

    pub fn move_position_relative(
        &mut self,
        index: usize,
        magnitude: isize,
    ) -> Result<(), OrderedTableError<T>> {
        self.check_bounds(index)?;

        let destination = index
            .checked_add_signed(magnitude)
            .ok_or(OrderedTableError::MoveOverflowError(index, magnitude))?;

        self.move_position(index, destination)
    }

    // This allows things like moving entries between active and archive tables,
    // modifying entries as they move across like-type tables,
    // and migrating to newer versions of a table using closures to convert types.
    // Could also do "migrate_all_from", and lay the groundwork for JOINs
    // TODO: add migrate_entry_from<U: TableEntry>(&mut self, source: &mut OrderedTable<U>, index, transform:
    // F) -> Result<(), OrderedTableError<T>>; where
    //     F: FnMut(&mut U) -> Result<T, T::ValidationError>,

    pub fn len(&self) -> usize { self.table.len() }

    pub fn is_empty(&self) -> bool { self.table.is_empty() }

    pub fn has_peristence(&self) -> bool { self.persistence.is_some() }

    pub fn iter(&self) -> impl Iterator<Item = &T> { self.table.iter() }

    /// Use this iterator to create your own sorted view of the [`OrderedTable`] that holds indices
    /// correspoding to the original table's indices for each entry.
    pub fn view_sorted<F>(&self, mut sort_fn: F) -> impl Iterator<Item = (usize, &T)>
    where
        F: FnMut(&T, &T) -> std::cmp::Ordering,
    {
        let mut indices: Vec<usize> = (0..self.len()).collect();
        indices.sort_by(|&a, &b| {
            let entry_a = &self.table[a];
            let entry_b = &self.table[b];
            sort_fn(entry_a, entry_b)
        });

        indices.into_iter().map(|orig_idx| (orig_idx, &self.table[orig_idx]))
    }

    pub fn separate_categories<F>(&mut self, mut category_fn: F)
    where
        F: FnMut(&T) -> u32,
    {
        self.table.sort_by(|a, b| category_fn(a).cmp(&category_fn(b)));
        self.save();
    }

    pub fn check_categories<F>(&self, mut category_fn: F) -> Result<(), OrderedTableError<T>>
    where
        F: FnMut(&T) -> u32,
    {
        if !self.table.is_sorted_by_key(|e| category_fn(e)) {
            return Err(OrderedTableError::CategoryError);
        }

        Ok(())
    }

    pub fn push_categorized<F>(&mut self, mut category_fn: F, entry: T) -> Result<(), OrderedTableError<T>>
    where
        F: FnMut(&T) -> u32,
    {
        self.check_categories(&mut category_fn)?;
        let entry_category = category_fn(&entry);
        let index =
            self.table.iter().position(|e| category_fn(e) > entry_category).unwrap_or(self.table.len());
        self.insert(index, entry)
    }

    pub fn move_position_categorized<F>(
        &mut self,
        mut category_fn: F,
        index: usize,
        destination: usize,
    ) -> Result<(), OrderedTableError<T>>
    where
        F: FnMut(&T) -> u32,
    {
        self.check_categories(&mut category_fn)?;
        self.check_bounds(index)?;
        self.check_bounds(destination)?;
        let current_category = category_fn(&self.table[index]);
        let destination_category = category_fn(&self.table[destination]);

        if current_category != destination_category {
            return Err(OrderedTableError::CategoryOutOfBoundsError(
                index,
                current_category,
                destination,
                destination_category,
            ));
        }

        self.move_position(index, destination)
    }
}

/// `CardSortMode` provides a convenient interface for sorting an [`OrderedTable`]
/// whose entries have a `label` and a `date`.
#[derive(
    Debug,
    thiserror::Error,
    Serialize,
    Deserialize,
    Clone,
    Copy,
    PartialEq,
    rkyv::Archive,
    rkyv::Serialize,
    rkyv::Deserialize,
)]
pub enum CardSortMode {
    #[error("by label alphabetically")]
    Label,
    #[error("by date added")]
    Date,
    #[error("by custom order")]
    Custom,
}

pub const CARD_SORT_MODES: [CardSortMode; 3] =
    [CardSortMode::Label, CardSortMode::Date, CardSortMode::Custom];

impl From<usize> for CardSortMode {
    fn from(value: usize) -> Self {
        if value >= CARD_SORT_MODES.len() {
            return CardSortMode::Custom;
        }

        CARD_SORT_MODES[value]
    }
}

pub trait SortableCard {
    fn get_label(&self) -> &String;
    fn get_date(&self) -> u64;

    fn compare_by(a: &Self, b: &Self, sort_mode: CardSortMode) -> std::cmp::Ordering {
        match sort_mode {
            CardSortMode::Label => a.get_label().to_lowercase().cmp(&b.get_label().to_lowercase()),
            CardSortMode::Date => a.get_date().cmp(&b.get_date()),
            CardSortMode::Custom => std::cmp::Ordering::Equal,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    impl TableEntry for String {
        type DuplicateReason = ();
        type ValidationError = ();

        fn is_duplicate(&self, other: &Self) -> Option<Self::DuplicateReason> {
            if self == other {
                return Some(());
            }
            None
        }

        fn validate(&self) -> Result<(), Self::ValidationError> {
            if self.is_empty() {
                return Err(());
            }
            Ok(())
        }
    }

    struct MockPersistence {
        save_counter: usize,
        data: String,
    }

    impl Persistence for MockPersistence {
        fn load(&mut self) -> Result<String, PersistenceError> { Ok(self.data.clone()) }

        fn save(&mut self, _table: &String) -> Result<(), PersistenceError> {
            self.save_counter += 1;
            Ok(())
        }
    }

    impl MockPersistence {
        fn new() -> Self { Self { save_counter: 0, data: String::from("[]") } }

        fn new_with_data(data: String) -> Self { Self { save_counter: 0, data } }
    }

    // String with non-reflexive is_duplicate
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct NRString(String);

    impl ToString for NRString {
        fn to_string(&self) -> String { self.0.clone() }
    }

    impl TableEntry for NRString {
        type DuplicateReason = ();
        type ValidationError = ();

        fn is_duplicate(&self, other: &Self) -> Option<Self::DuplicateReason> {
            if self.0 == other.0.to_lowercase() {
                return Some(());
            }
            None
        }

        fn validate(&self) -> Result<(), Self::ValidationError> {
            if self.0.is_empty() {
                return Err(());
            }
            Ok(())
        }
    }

    // String with non-deterministic is_duplicate
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct NDString(String);

    static ND_COUNTER: AtomicUsize = AtomicUsize::new(0);

    impl ToString for NDString {
        fn to_string(&self) -> String { self.0.clone() }
    }

    impl TableEntry for NDString {
        type DuplicateReason = ();
        type ValidationError = ();

        fn is_duplicate(&self, other: &Self) -> Option<Self::DuplicateReason> {
            let counter = ND_COUNTER.fetch_add(1, Ordering::SeqCst);
            if self.0 == other.0 && (counter == 1 || counter % 2 == 0) {
                return Some(());
            }
            None
        }

        fn validate(&self) -> Result<(), Self::ValidationError> {
            if self.0.is_empty() {
                return Err(());
            }
            Ok(())
        }
    }

    #[derive(Debug, Serialize, Deserialize, Clone)]
    pub struct Card {
        label: String,
        date: u64,
    }

    impl SortableCard for Card {
        fn get_label(&self) -> &String { &self.label }

        fn get_date(&self) -> u64 { self.date }
    }

    fn card1() -> Card { Card { label: String::from("A"), date: 0 } }

    fn card2() -> Card { Card { label: String::from("B"), date: 0 } }

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("Tables are different lengths: {0:?}, {1:?}")]
        DifferentLengthsError(usize, usize),
        #[error("Entries differ at these indices: {0:?}")]
        DifferentEntriesError(Vec<usize>),
    }

    fn list1() -> Result<OrderedTable<String, MockPersistence>, OrderedTableError<String>> {
        let string = String::from("A");
        let mut list =
            OrderedTable::<String, MockPersistence>::new().with_persistence(MockPersistence::new())?;
        list.push(string)?;
        Ok(list)
    }

    fn list2() -> Result<OrderedTable<String, MockPersistence>, OrderedTableError<String>> {
        let string = String::from("B");
        let mut list = list1()?;
        list.push(string)?;
        Ok(list)
    }

    fn list3() -> Result<OrderedTable<String, MockPersistence>, OrderedTableError<String>> {
        let string = String::from("C");
        let mut list = list2()?;
        list.push(string)?;
        Ok(list)
    }

    fn list3_alt() -> Result<OrderedTable<String, MockPersistence>, OrderedTableError<String>> {
        let string = String::from("D");
        let mut list = list2()?;
        list.push(string)?;
        Ok(list)
    }

    fn edit_where_list() -> Result<OrderedTable<String, MockPersistence>, OrderedTableError<String>> {
        let mut list =
            OrderedTable::<String, MockPersistence>::new().with_persistence(MockPersistence::new())?;
        list.push(String::from("A"))?;
        list.push(String::from("AA"))?;
        list.push(String::from("B"))?;
        list.push(String::from("BB"))?;
        Ok(list)
    }

    fn edit_where_result_list() -> Result<OrderedTable<String, MockPersistence>, OrderedTableError<String>> {
        let mut list =
            OrderedTable::<String, MockPersistence>::new().with_persistence(MockPersistence::new())?;
        list.push(String::from("AC"))?;
        list.push(String::from("AA"))?;
        list.push(String::from("BC"))?;
        list.push(String::from("BB"))?;
        Ok(list)
    }

    fn table_diff_exclude_index(
        a: &OrderedTable<String, MockPersistence>,
        b: &OrderedTable<String, MockPersistence>,
        index: Option<usize>,
    ) -> Result<(), TestError> {
        let len_a = a.len();
        let len_b = b.len();

        if len_a != len_b {
            return Err(TestError::DifferentLengthsError(len_a, len_b));
        }

        let mut differing_indices = Vec::<usize>::new();
        for (i, e) in a.iter().enumerate().filter(|(i, _)| index.is_none_or(|idx| *i != idx)) {
            if *e != b.table[i] {
                differing_indices.push(i);
            }
        }

        if !differing_indices.is_empty() {
            return Err(TestError::DifferentEntriesError(differing_indices));
        }

        Ok(())
    }

    fn table_diff(
        a: &OrderedTable<String, MockPersistence>,
        b: &OrderedTable<String, MockPersistence>,
    ) -> Result<(), TestError> {
        table_diff_exclude_index(a, b, None)
    }

    #[test]
    fn test_table_diff() {
        let a = list3().unwrap();
        let b = list3().unwrap();
        table_diff(&a, &b).unwrap();
    }

    #[test]
    fn test_table_diff_len() {
        let list2 = list2().unwrap();
        let list3 = list3().unwrap();
        match table_diff(&list2, &list3) {
            Ok(_) => panic!("table_diff should flag tables with different lengths"),
            Err(TestError::DifferentLengthsError(a, b)) if a == 2 && b == 3 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn test_table_diff_indices() {
        let a = list3().unwrap();
        let b = list3_alt().unwrap();
        match table_diff(&a, &b) {
            Ok(_) => panic!("table_diff should flag tables with different lengths"),
            Err(TestError::DifferentEntriesError(diffs)) if diffs.len() == 1 && diffs[0] == 2 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn check_reflexive() {
        let list =
            OrderedTable::<String, MockPersistence>::new().with_persistence(MockPersistence::new()).unwrap();
        let label = String::from("A");
        list.check_reflexive(None, &label).unwrap();
    }

    #[test]
    fn with_persistence() {
        OrderedTable::<String, MockPersistence>::new()
            .with_persistence(MockPersistence::new_with_data(String::from("[\"A\"]")))
            .unwrap();
    }

    #[test]
    fn with_persistence_duplicate() {
        match OrderedTable::<String, MockPersistence>::new()
            .with_persistence(MockPersistence::new_with_data(String::from("[\"A\", \"A\"]")))
        {
            Ok(_) => panic!("Loading a duplicate item should fail."),
            Err(OrderedTableError::LoadDuplicateError(i, (_, other))) if i == 0 && other == 1 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn with_persistence_invalid() {
        match OrderedTable::<String, MockPersistence>::new()
            .with_persistence(MockPersistence::new_with_data(String::from("[\"A\", \"\"]")))
        {
            Ok(_) => panic!("Loading an invalid item should fail."),
            Err(OrderedTableError::LoadInvalidError(i, _)) if i == 1 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn check_reflexive_nr() {
        let list = OrderedTable::<NRString, MockPersistence>::new()
            .with_persistence(MockPersistence::new())
            .unwrap();
        let label = NRString(String::from("A"));
        match list.check_reflexive(None, &label) {
            Ok(_) => panic!("Reflexivity on a non-reflexive is_duplicate should fail."),
            Err(OrderedTableError::NonReflexiveError(index, left_self, right_self))
                if index.is_none() && left_self.is_none() && right_self.is_none() =>
            {
                ()
            }
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn with_persistence_nr() {
        match OrderedTable::<NRString, MockPersistence>::new()
            .with_persistence(MockPersistence::new_with_data(String::from("[\"A\"]")))
        {
            Ok(_) => panic!("Reflexivity on a non-reflexive is_duplicate should fail."),
            Err(OrderedTableError::NonReflexiveError(index, left_self, right_self))
                if index.unwrap() == 0 && left_self.is_none() && right_self.is_none() =>
            {
                ()
            }
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn list_of_1() {
        let list = list1().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list.persistence.unwrap().save_counter, 1);
    }

    #[test]
    fn list_of_2() {
        let list = list2().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list.persistence.unwrap().save_counter, 2);
    }

    #[test]
    fn find_exclude_index_none() {
        let list = list3().unwrap();
        let label = String::from("B");
        let (_, i) = list.find_exclude_index(&label, None).unwrap().unwrap();
        assert_eq!(i, 1);
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn find_exclude_index_of_entry() {
        let list = list3().unwrap();
        let label = String::from("B");
        match list.find_exclude_index(&label, Some(1)).unwrap() {
            Some(_) => panic!("Finding an entry whose index is excluded should fail."),
            None => (),
        }
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn find_exclude_index_of_other_entry() {
        let list = list3().unwrap();
        let label = String::from("B");
        let (_, i) = list.find_exclude_index(&label, Some(0)).unwrap().unwrap();
        assert_eq!(i, 1);
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn find_exclude_index_none_non_reflexive() {
        let list = OrderedTable::<NRString, MockPersistence>::new()
            .with_persistence(MockPersistence::new())
            .unwrap();
        let label = NRString(String::from("B"));
        match list.find_exclude_index(&label, None) {
            Ok(_) => panic!("Encountering a non-reflexive is_duplicate should fail."),
            Err(OrderedTableError::NonReflexiveError(index, left_self, _right_self))
                if index.is_none() && left_self.is_none() =>
            {
                ()
            }
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        assert_eq!(list.persistence.unwrap().save_counter, 0);
    }

    #[test]
    fn find_exclude_index_none_non_deterministic() {
        let mut list = OrderedTable::<NDString, MockPersistence>::new()
            .with_persistence(MockPersistence::new())
            .unwrap();
        let label = NDString(String::from("B"));
        list.push(label.clone()).unwrap();
        match list.find_exclude_index(&label, None) {
            Ok(_) => panic!("Encountering a non-deterministic is_duplicate should fail."),
            Err(OrderedTableError::NonDeterministicError(
                index,
                left_self,
                right_self,
                left_match,
                right_match,
            )) if index.unwrap() == 0
                && left_self.is_some()
                && right_self.is_some()
                && left_match.is_none()
                && right_match.is_some() =>
            {
                ()
            }
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        assert_eq!(list.persistence.unwrap().save_counter, 1);
    }

    #[test]
    fn find_existing() {
        let list = list1().unwrap();
        let label = String::from("A");
        let (_, i) = list.find(&label).unwrap().unwrap();
        assert_eq!(i, 0);
        assert_eq!(list.persistence.unwrap().save_counter, 1);
    }

    #[test]
    fn find_nonexistant() {
        let list = list1().unwrap();
        let label = String::from("Z");
        match list.find(&label).unwrap() {
            Some(_) => panic!("Finding a non-existant entry should fail."),
            None => (),
        }
        assert_eq!(list.persistence.unwrap().save_counter, 1);
    }

    #[test]
    fn push_invalid() {
        let mut list =
            OrderedTable::<String, MockPersistence>::new().with_persistence(MockPersistence::new()).unwrap();
        let label = String::new();
        match list.push(label) {
            Ok(_) => panic!("Pushing an invalid entry should fail."),
            Err(OrderedTableError::PushInvalidError(_)) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        assert_eq!(list.len(), 0);
        assert_eq!(list.persistence.unwrap().save_counter, 0);
    }

    #[test]
    fn push_duplicate() {
        let mut list = list1().unwrap();
        let label = String::from("A");
        match list.push(label) {
            Ok(_) => panic!("Pushing a duplicate item should fail."),
            Err(OrderedTableError::PushDuplicateError(_)) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        assert_eq!(list.len(), 1);
        assert_eq!(list.persistence.unwrap().save_counter, 1);
    }

    #[test]
    fn insert() {
        let mut list = list2().unwrap();
        let label = String::from("C");
        list.insert(1, label).unwrap();
        assert_eq!(list.table[0], String::from("A"));
        assert_eq!(list.table[1], String::from("C"));
        assert_eq!(list.table[2], String::from("B"));
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn insert_end() {
        let mut list = list2().unwrap();
        let label = String::from("C");
        list.insert(2, label).unwrap();
        assert_eq!(list.table[0], String::from("A"));
        assert_eq!(list.table[1], String::from("B"));
        assert_eq!(list.table[2], String::from("C"));
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn insert_out_of_bounds() {
        let mut list = list2().unwrap();
        let label = String::from("C");

        match list.insert(3, label) {
            Ok(_) => panic!("Going out of bounds should fail."),
            Err(OrderedTableError::OutOfBoundsError(a, b)) if a == 3 && b == 2 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        assert_eq!(list.persistence.unwrap().save_counter, 2);
    }

    #[test]
    fn check_bounds() {
        let list = list3().unwrap();
        list.check_bounds(2).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn check_bounds_out_of_bounds() {
        let list = list3().unwrap();
        match list.check_bounds(3) {
            Ok(_) => panic!("Going out of bounds should fail."),
            Err(OrderedTableError::OutOfBoundsError(a, b)) if a == 3 && b == 3 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn remove() {
        let mut list = list3().unwrap();
        let label = list.remove(1).unwrap();
        assert_eq!(label, String::from("B"));
        assert_eq!(list.len(), 2);
        assert_eq!(list.table[0], String::from("A"));
        assert_eq!(list.table[1], String::from("C"));
        assert_eq!(list.persistence.unwrap().save_counter, 4);
    }

    #[test]
    fn remove_out_of_bounds() {
        let mut list = list3().unwrap();
        match list.remove(3) {
            Ok(_) => panic!("Removing an entry out of bounds should fail."),
            Err(OrderedTableError::OutOfBoundsError(a, b)) if a == 3 && b == 3 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        assert_eq!(list.len(), 3);
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn get() {
        let list = list3().unwrap();
        let label_ref = list.get(1).unwrap();
        assert_eq!(label_ref, &list.table[1]);
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn get_out_of_bounds() {
        let list = list3().unwrap();
        match list.get(3) {
            Ok(_) => panic!("Getting an entry out of bounds should fail."),
            Err(OrderedTableError::OutOfBoundsError(a, b)) if a == 3 && b == 3 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn validate_edit() {
        let list = list3().unwrap();
        list.validate_edit(0, |entry| {
            *entry = String::from("Z");
            Ok(())
        })
        .unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn validate_edit_out_of_bounds() {
        let list = list3().unwrap();
        match list.validate_edit(3, |entry| {
            *entry = String::from("Z");
            Ok(())
        }) {
            Ok(_) => panic!("Editing an entry out of bounds should fail."),
            Err(OrderedTableError::OutOfBoundsError(a, b)) if a == 3 && b == 3 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn validate_edit_invalid_operation() {
        let list = list3().unwrap();
        match list.validate_edit(0, |_entry| Err(())) {
            Ok(_) => panic!("Performing an invalid edit operation should fail."),
            Err(OrderedTableError::EditInvalidOperationError(_)) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn validate_edit_invalid_result() {
        let list = list3().unwrap();
        match list.validate_edit(0, |entry| {
            *entry = String::new();
            Ok(())
        }) {
            Ok(_) => panic!("Creating an invalid entry should fail."),
            Err(OrderedTableError::EditInvalidResultError(_)) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn validate_edit_duplicate_result() {
        let list = list3().unwrap();
        match list.validate_edit(0, |entry| {
            *entry = String::from("B");
            Ok(())
        }) {
            Ok(_) => panic!("Creating a duplicate entry should fail."),
            Err(OrderedTableError::EditDuplicateError((_, i))) if i == 1 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn edit() {
        let mut list = list3().unwrap();
        list.edit(0, |entry| {
            *entry = String::from("Z");
            Ok(())
        })
        .unwrap();
        let fresh_list = list3().unwrap();
        table_diff_exclude_index(&list, &fresh_list, Some(0)).unwrap();
        assert_eq!(list.table[0], String::from("Z"));
        assert_eq!(list.persistence.unwrap().save_counter, 4);
    }

    #[test]
    fn edit_out_of_bounds() {
        let mut list = list3().unwrap();
        match list.edit(3, |entry| {
            *entry = String::from("Z");
            Ok(())
        }) {
            Ok(_) => panic!("Editing an entry out of bounds should fail."),
            Err(OrderedTableError::OutOfBoundsError(a, b)) if a == 3 && b == 3 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        let fresh_list = list3().unwrap();
        table_diff(&list, &fresh_list).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn edit_invalid_operation() {
        let mut list = list3().unwrap();
        match list.edit(0, |_entry| Err(())) {
            Ok(_) => panic!("Performing an invalid edit operation should fail."),
            Err(OrderedTableError::EditInvalidOperationError(_)) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        let fresh_list = list3().unwrap();
        table_diff(&list, &fresh_list).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn edit_invalid_result() {
        let mut list = list3().unwrap();
        match list.edit(0, |entry| {
            *entry = String::new();
            Ok(())
        }) {
            Ok(_) => panic!("Creating an invalid entry should fail."),
            Err(OrderedTableError::EditInvalidResultError(_)) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        let fresh_list = list3().unwrap();
        table_diff(&list, &fresh_list).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn edit_duplicate_result() {
        let mut list = list3().unwrap();
        match list.edit(0, |entry| {
            *entry = String::from("B");
            Ok(())
        }) {
            Ok(_) => panic!("Creating a dupicate entry should fail."),
            Err(OrderedTableError::EditDuplicateError((_, i))) if i == 1 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        let fresh_list = list3().unwrap();
        table_diff(&list, &fresh_list).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn edit_where() {
        let mut list = edit_where_list().unwrap();
        list.edit_where(
            |entry| {
                entry.push('C');
                Ok(())
            },
            |entry| entry.len() == 1,
        )
        .unwrap();
        let result_list = edit_where_result_list().unwrap();
        table_diff(&list, &result_list).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 5);
    }

    #[test]
    fn edit_where_invalid_operation() {
        let mut list = edit_where_list().unwrap();
        match list.edit_where(|_entry| Err(()), |entry| entry.len() == 1) {
            Ok(_) => panic!("Mass editing with an invalid operation should fail."),
            Err(errors_vec) => {
                for (i, e) in errors_vec.iter() {
                    match e {
                        OrderedTableError::EditInvalidOperationError(_) => (),
                        other => panic!("Failed with the wrong error at index {}: {}", i, other),
                    }
                }
            }
        }
        let fresh_list = edit_where_list().unwrap();
        table_diff(&list, &fresh_list).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 4);
    }

    #[test]
    fn edit_where_invalid_results() {
        let mut list = edit_where_list().unwrap();
        match list.edit_where(
            |entry| {
                entry.pop().ok_or(())?;
                Ok(())
            },
            |entry| entry.len() == 1,
        ) {
            Ok(_) => panic!("Mass editing with an invalid result should fail."),
            Err(errors_vec) => {
                for (i, e) in errors_vec.iter() {
                    match e {
                        OrderedTableError::EditInvalidResultError(_) => (),
                        other => panic!("Failed with the wrong error at index {}: {}", i, other),
                    }
                }
            }
        }
        let fresh_list = edit_where_list().unwrap();
        table_diff(&list, &fresh_list).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 4);
    }

    #[test]
    fn edit_where_duplicate_results() {
        let mut list = edit_where_list().unwrap();
        match list.edit_where(
            |entry| {
                entry.pop().ok_or(())?;
                entry.push('A');
                Ok(())
            },
            |entry| entry.len() == 1,
        ) {
            Ok(_) => panic!("Mass editing with a duplicate result should fail."),
            Err(errors_vec) => {
                assert_eq!(errors_vec.len(), 1);
                for (i, e) in errors_vec.iter() {
                    match e {
                        OrderedTableError::EditDuplicateError((_, idx)) if *idx == 0 => (),
                        other => panic!("Failed with the wrong error at index {}: {}", i, other),
                    }
                }
            }
        }
        let fresh_list = edit_where_list().unwrap();
        table_diff(&list, &fresh_list).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 4);
    }

    #[test]
    fn move_position_end() {
        let mut list = list3().unwrap();
        list.move_position(0, list.len() - 1).unwrap();
        assert_eq!(list.table[0], String::from("B"));
        assert_eq!(list.table[1], String::from("C"));
        assert_eq!(list.table[2], String::from("A"));
        assert_eq!(list.persistence.unwrap().save_counter, 4);
    }

    #[test]
    fn move_position_relative_end() {
        let mut list = list3().unwrap();
        list.move_position_relative(0, 2).unwrap();
        assert_eq!(list.table[0], String::from("B"));
        assert_eq!(list.table[1], String::from("C"));
        assert_eq!(list.table[2], String::from("A"));
        assert_eq!(list.persistence.unwrap().save_counter, 4);
    }

    #[test]
    fn move_position_relative_before() {
        let mut list = list3().unwrap();
        list.move_position_relative(1, -1).unwrap();
        assert_eq!(list.table[0], String::from("B"));
        assert_eq!(list.table[1], String::from("A"));
        assert_eq!(list.table[2], String::from("C"));
        assert_eq!(list.persistence.unwrap().save_counter, 4);
    }

    #[test]
    fn move_position_relative_after() {
        let mut list = list3().unwrap();
        list.move_position_relative(0, 1).unwrap();
        assert_eq!(list.table[0], String::from("B"));
        assert_eq!(list.table[1], String::from("A"));
        assert_eq!(list.table[2], String::from("C"));
        assert_eq!(list.persistence.unwrap().save_counter, 4);
    }

    #[test]
    fn move_position_relative_zero() {
        let mut list = list3().unwrap();
        list.move_position_relative(0, 0).unwrap();
        let fresh_list = list3().unwrap();
        table_diff(&list, &fresh_list).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn move_position_relative_from_out_of_bounds() {
        let mut list = list3().unwrap();
        match list.move_position_relative(3, -1) {
            Ok(_) => panic!("Moving an entry that is out of bounds should fail."),
            Err(OrderedTableError::OutOfBoundsError(a, b)) if a == 3 && b == 3 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        let fresh_list = list3().unwrap();
        table_diff(&list, &fresh_list).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn move_position_relative_underflow() {
        let mut list = list3().unwrap();
        match list.move_position_relative(0, -1) {
            Ok(_) => panic!("Moving an entry to a negative position should fail."),
            Err(OrderedTableError::MoveOverflowError(index, magnitude)) if index == 0 && magnitude == -1 => {
                ()
            }
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        let fresh_list = list3().unwrap();
        table_diff(&list, &fresh_list).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn move_position_relative_to_out_of_bounds() {
        let mut list = list3().unwrap();
        match list.move_position_relative(2, 1) {
            Ok(_) => panic!("Moving an entry out of bounds should fail."),
            Err(OrderedTableError::OutOfBoundsError(a, b)) if a == 3 && b == 3 => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
        let fresh_list = list3().unwrap();
        table_diff(&list, &fresh_list).unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 3);
    }

    #[test]
    fn view_sorted() {
        let list = list3().unwrap();
        let view: Vec<(usize, &String)> = list.view_sorted(|a, b| b.cmp(a)).collect();
        assert_eq!(view[0], (2, &list.table[2]));
        assert_eq!(view[1], (1, &list.table[1]));
        assert_eq!(view[2], (0, &list.table[0]));
    }

    #[test]
    fn view_sorted_nop() {
        let list = list3().unwrap();
        let view: Vec<(usize, &String)> = list.view_sorted(|_a, _b| std::cmp::Ordering::Equal).collect();
        assert_eq!(view[0], (0, &list.table[0]));
        assert_eq!(view[1], (1, &list.table[1]));
        assert_eq!(view[2], (2, &list.table[2]));
    }

    #[test]
    fn view_sorted_empty() {
        let list = OrderedTable::<String, MockPersistence>::new();
        let view: Vec<(usize, &String)> = list.view_sorted(|a, b| b.cmp(a)).collect();
        assert!(view.is_empty());
    }

    #[test]
    fn separate_and_check_categories() {
        let mut list = list3().unwrap();
        list.separate_categories(
            |e| if e.cmp(&String::from("B")) == std::cmp::Ordering::Less { 1 } else { 0 },
        );
        assert_eq!(list.table[0], String::from("B"));
        assert_eq!(list.table[1], String::from("C"));
        assert_eq!(list.table[2], String::from("A"));
        list.check_categories(|e| if e.cmp(&String::from("B")) == std::cmp::Ordering::Less { 1 } else { 0 })
            .unwrap();
        assert_eq!(list.persistence.unwrap().save_counter, 4);
    }

    #[test]
    fn check_categories_unseparated() {
        let list = list3().unwrap();
        match list
            .check_categories(|e| if e.cmp(&String::from("B")) == std::cmp::Ordering::Less { 1 } else { 0 })
        {
            Ok(_) => panic!("Checking categories on an uncategorized table should fail."),
            Err(OrderedTableError::CategoryError) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }

    #[test]
    fn push_categorized() {
        let mut list = list3().unwrap();
        list.separate_categories(
            |e| if e.cmp(&String::from("B")) == std::cmp::Ordering::Less { 1 } else { 0 },
        );
        list.push_categorized(
            |e| if e.cmp(&String::from("B")) == std::cmp::Ordering::Less { 1 } else { 0 },
            String::from("D"),
        )
        .unwrap();
        assert_eq!(list.table[0], String::from("B"));
        assert_eq!(list.table[1], String::from("C"));
        assert_eq!(list.table[2], String::from("D"));
        assert_eq!(list.table[3], String::from("A"));
        assert_eq!(list.persistence.unwrap().save_counter, 5);
    }

    #[test]
    fn move_position_categorized() {
        let mut list = list3().unwrap();
        list.separate_categories(
            |e| if e.cmp(&String::from("B")) == std::cmp::Ordering::Less { 1 } else { 0 },
        );
        list.move_position_categorized(
            |e| if e.cmp(&String::from("B")) == std::cmp::Ordering::Less { 1 } else { 0 },
            0,
            1,
        )
        .unwrap();
        assert_eq!(list.table[0], String::from("C"));
        assert_eq!(list.table[1], String::from("B"));
        assert_eq!(list.table[2], String::from("A"));
        assert_eq!(list.persistence.unwrap().save_counter, 5);
    }

    #[test]
    fn move_position_categorized_out_of_bounds() {
        let mut list = list3().unwrap();
        list.separate_categories(
            |e| if e.cmp(&String::from("B")) == std::cmp::Ordering::Less { 1 } else { 0 },
        );
        match list.move_position_categorized(
            |e| if e.cmp(&String::from("B")) == std::cmp::Ordering::Less { 1 } else { 0 },
            1,
            2,
        ) {
            Ok(_) => panic!("Moving an entry out of its category should fail."),
            Err(OrderedTableError::CategoryOutOfBoundsError(..)) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }

        assert_eq!(list.persistence.unwrap().save_counter, 4);
    }

    #[test]
    fn sort_mode_from_usize() {
        assert_eq!(CardSortMode::Label, CardSortMode::from(0));
        assert_eq!(CardSortMode::Date, CardSortMode::from(1));
        assert_eq!(CardSortMode::Custom, CardSortMode::from(2));
        assert_eq!(CardSortMode::Custom, CardSortMode::from(4));
    }

    #[test]
    fn compare_label() {
        let a = card1();
        let b = card2();
        assert_eq!(std::cmp::Ordering::Equal, Card::compare_by(&a, &a, CardSortMode::Label));
        assert_eq!(std::cmp::Ordering::Less, Card::compare_by(&a, &b, CardSortMode::Label));
        assert_eq!(std::cmp::Ordering::Greater, Card::compare_by(&b, &a, CardSortMode::Label));
    }

    #[test]
    fn compare_date() {
        let mut a = card1();
        let b = card2();
        a.date = 1;
        assert_eq!(std::cmp::Ordering::Equal, Card::compare_by(&a, &a, CardSortMode::Date));
        assert_eq!(std::cmp::Ordering::Greater, Card::compare_by(&a, &b, CardSortMode::Date));
        assert_eq!(std::cmp::Ordering::Less, Card::compare_by(&b, &a, CardSortMode::Date));
    }

    #[test]
    fn compare_custom() {
        let mut a = card1();
        let b = card2();
        a.date = 1;
        assert_eq!(std::cmp::Ordering::Equal, Card::compare_by(&a, &a, CardSortMode::Custom));
        assert_eq!(std::cmp::Ordering::Equal, Card::compare_by(&a, &b, CardSortMode::Custom));
        assert_eq!(std::cmp::Ordering::Equal, Card::compare_by(&b, &a, CardSortMode::Custom));
    }

    // String with assymetric duplicate reason
    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct AsymmetricString(String);

    impl ToString for AsymmetricString {
        fn to_string(&self) -> String { self.0.clone() }
    }

    #[derive(Debug, thiserror::Error)]
    enum AsymmetricReason {
        #[error("Same length: {0:?}")]
        SameLength(usize),
        #[error("Same first character: {0:?}")]
        SameFirstChar(String),
    }

    impl TableEntry for AsymmetricString {
        type DuplicateReason = AsymmetricReason;
        type ValidationError = ();

        fn is_duplicate(&self, other: &Self) -> Option<Self::DuplicateReason> {
            if self.0.len() == other.0.len() {
                return Some(AsymmetricReason::SameLength(self.0.len()));
            }

            let self_first_char = self.0.chars().next().unwrap();
            if self_first_char == other.0.chars().next().unwrap() {
                return Some(AsymmetricReason::SameFirstChar(String::from(self_first_char)));
            }
            None
        }

        fn validate(&self) -> Result<(), Self::ValidationError> {
            if self.0.is_empty() {
                return Err(());
            }
            Ok(())
        }
    }

    #[test]
    fn accept_shallow_matching() {
        let mut list = OrderedTable::<AsymmetricString, MockPersistence>::new()
            .with_persistence(MockPersistence::new())
            .unwrap();
        let label1 = AsymmetricString(String::from("A"));
        let label2 = AsymmetricString(String::from("Apple"));

        match label1.is_duplicate(&label1).unwrap() {
            AsymmetricReason::SameLength(l) if l == 1 => (),
            other => panic!("Failed with the wrong error: {}", other),
        }

        match label2.is_duplicate(&label2).unwrap() {
            AsymmetricReason::SameLength(l) if l == 5 => (),
            other => panic!("Failed with the wrong error: {}", other),
        }

        match label1.is_duplicate(&label2).unwrap() {
            AsymmetricReason::SameFirstChar(c) if c == String::from("A") => (),
            other => panic!("Failed with the wrong error: {}", other),
        }

        list.push(label1).unwrap();
        match list.push(label2) {
            Ok(_) => panic!("Pushing a duplicate item should fail."),
            Err(OrderedTableError::PushDuplicateError((AsymmetricReason::SameFirstChar(_), _))) => (),
            Err(other) => panic!("Failed with the wrong error: {}", other),
        }
    }
}
