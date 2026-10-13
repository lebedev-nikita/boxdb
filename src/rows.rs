//! Stable row slots with page and row copy-on-write. Cloning a transaction or
//! savepoint copies page pointers; a point update copies only its page and row.
use crate::{Error, Result, Value};
use std::{
    ops::{Index, IndexMut},
    sync::Arc,
};

const PAGE_ROWS: usize = 128;
#[derive(Clone, Debug)]
struct Slot {
    id: u64,
    row: Option<Arc<Vec<Value>>>,
}
#[derive(Clone, Debug)]
pub(crate) struct Rows {
    pages: Vec<Arc<Vec<Slot>>>,
    slots: usize,
    len: usize,
    lineage: Arc<()>,
}
impl Default for Rows {
    fn default() -> Self {
        Self {
            pages: vec![],
            slots: 0,
            len: 0,
            lineage: Arc::new(()),
        }
    }
}
pub(crate) struct Change<'a> {
    pub position: usize,
    pub id: u64,
    pub old: Option<&'a Vec<Value>>,
    pub new: Option<&'a Vec<Value>>,
}
impl Rows {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn last_id(&self) -> u64 {
        self.pages.last().and_then(|p| p.last()).map_or(0, |s| s.id)
    }
    pub fn shares_history(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.lineage, &other.lineage)
    }
    pub fn push(&mut self, id: u64, row: Vec<Value>) -> Result<()> {
        if id == 0 || id <= self.last_id() {
            return Err(Error("invalid or duplicate row id".into()));
        }
        if self.slots.is_multiple_of(PAGE_ROWS) {
            self.pages.push(Arc::new(Vec::with_capacity(PAGE_ROWS)));
        }
        Arc::make_mut(self.pages.last_mut().unwrap()).push(Slot {
            id,
            row: Some(Arc::new(row)),
        });
        self.slots += 1;
        self.len += 1;
        Ok(())
    }
    pub fn positions(&self) -> impl Iterator<Item = (usize, &Vec<Value>)> {
        self.pages.iter().enumerate().flat_map(|(page, slots)| {
            slots.iter().enumerate().filter_map(move |(slot, row)| {
                row.row.as_deref().map(|r| (page * PAGE_ROWS + slot, r))
            })
        })
    }
    pub fn iter(&self) -> impl Iterator<Item = &Vec<Value>> {
        self.positions().map(|(_, row)| row)
    }
    pub fn records(&self) -> impl Iterator<Item = (u64, &Vec<Value>)> {
        self.pages.iter().flat_map(|page| {
            page.iter()
                .filter_map(|slot| slot.row.as_deref().map(|row| (slot.id, row)))
        })
    }
    pub fn id(&self, position: usize) -> u64 {
        self.slot(position).unwrap().id
    }
    fn slot(&self, position: usize) -> Option<&Slot> {
        self.pages
            .get(position / PAGE_ROWS)
            .and_then(|p| p.get(position % PAGE_ROWS))
    }
    pub fn find(&self, id: u64) -> Option<usize> {
        let (mut low, mut high) = (0, self.slots);
        while low < high {
            let mid = low + (high - low) / 2;
            if self.id(mid) < id {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        self.slot(low)
            .filter(|s| s.id == id && s.row.is_some())
            .map(|_| low)
    }
    pub fn remove(&mut self, position: usize) {
        if Arc::make_mut(&mut self.pages[position / PAGE_ROWS])[position % PAGE_ROWS]
            .row
            .take()
            .is_some()
        {
            self.len -= 1;
        }
    }
    pub fn changes<'a>(&'a self, previous: &'a Self) -> Vec<Change<'a>> {
        let mut changes = Vec::new();
        for page in 0..self.pages.len().max(previous.pages.len()) {
            if let (Some(a), Some(b)) = (self.pages.get(page), previous.pages.get(page))
                && Arc::ptr_eq(a, b)
            {
                continue;
            }
            let end = ((page + 1) * PAGE_ROWS).min(self.slots.max(previous.slots));
            for position in page * PAGE_ROWS..end {
                let new = self.slot(position);
                let old = previous.slot(position);
                if let (Some(a), Some(b)) = (new, old) {
                    match (&a.row, &b.row) {
                        (Some(a), Some(b)) if Arc::ptr_eq(a, b) => continue,
                        (None, None) => continue,
                        _ => (),
                    }
                }
                let old_row = old.and_then(|s| s.row.as_deref());
                let new_row = new.and_then(|s| s.row.as_deref());
                if old_row.is_none() && new_row.is_none() {
                    continue;
                }
                changes.push(Change {
                    position,
                    id: new.or(old).unwrap().id,
                    old: old_row,
                    new: new_row,
                });
            }
        }
        changes
    }
    pub fn compact(&mut self) {
        if self.slots <= 4096 || self.slots - self.len <= self.len {
            return;
        }
        let slots = self
            .pages
            .iter()
            .flat_map(|p| p.iter())
            .filter(|s| s.row.is_some())
            .cloned()
            .collect::<Vec<_>>();
        self.pages = slots
            .chunks(PAGE_ROWS)
            .map(|p| Arc::new(p.to_vec()))
            .collect();
        self.slots = self.len;
        self.lineage = Arc::new(());
    }
}
impl Index<usize> for Rows {
    type Output = Vec<Value>;
    fn index(&self, position: usize) -> &Self::Output {
        self.slot(position).unwrap().row.as_deref().unwrap()
    }
}
impl IndexMut<usize> for Rows {
    fn index_mut(&mut self, position: usize) -> &mut Self::Output {
        let slot = &mut Arc::make_mut(&mut self.pages[position / PAGE_ROWS])[position % PAGE_ROWS];
        Arc::make_mut(slot.row.as_mut().unwrap())
    }
}
