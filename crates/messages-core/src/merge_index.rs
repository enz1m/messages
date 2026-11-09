use std::{collections::HashMap, hash::Hash};

use crate::ChatMessage;
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct MergeIndex {
    ids: Positions<String>,
    transactions: Positions<Uuid>,
}
struct Positions<K>(HashMap<K, Vec<usize>>);
impl<K> Default for Positions<K> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}
impl<K: Eq + Hash> Positions<K> {
    fn first(&self, key: &K) -> Option<usize> {
        self.0
            .get(key)
            .and_then(|positions| positions.first().copied())
    }
    fn insert(&mut self, key: K, index: usize) {
        let positions = self.0.entry(key).or_default();
        if let Err(position) = positions.binary_search(&index) {
            positions.insert(position, index);
        }
    }
    fn remove(&mut self, key: &K, index: usize) {
        if let Some(positions) = self.0.get_mut(key) {
            if let Ok(position) = positions.binary_search(&index) {
                positions.remove(position);
            }
            if positions.is_empty() {
                self.0.remove(key);
            }
        }
    }
}
impl MergeIndex {
    pub(crate) fn new(messages: &[ChatMessage]) -> Self {
        let mut index = Self::default();
        for (position, message) in messages.iter().enumerate() {
            index.insert(message, position);
        }
        index
    }
    pub(crate) fn find(&self, message: &ChatMessage) -> Option<usize> {
        // ID and transaction aliases can disagree; the earliest stored match owns settlement.
        self.ids
            .first(&message.id)
            .into_iter()
            .chain(
                message
                    .transaction
                    .and_then(|transaction| self.transactions.first(&transaction)),
            )
            .min()
    }
    pub(crate) fn insert(&mut self, message: &ChatMessage, position: usize) {
        self.ids.insert(message.id.clone(), position);
        if let Some(transaction) = message.transaction {
            self.transactions.insert(transaction, position);
        }
    }
    pub(crate) fn replace(&mut self, old: &ChatMessage, new: &ChatMessage, position: usize) {
        if old.id != new.id {
            self.ids.remove(&old.id, position);
            self.ids.insert(new.id.clone(), position);
        }
        if old.transaction != new.transaction {
            if let Some(transaction) = old.transaction {
                self.transactions.remove(&transaction, position);
            }
            if let Some(transaction) = new.transaction {
                self.transactions.insert(transaction, position);
            }
        }
    }
}
