//! Ephemeral session-scoped resource snapshots, not a conversation store.
use std::collections::VecDeque;
pub struct TextCache {
    limit: usize,
    size: usize,
    items: VecDeque<((String, String), String, usize)>,
}
impl TextCache {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            size: 0,
            items: VecDeque::new(),
        }
    }
    pub fn get(&mut self, session: &str, resource: &str) -> Option<String> {
        let index = self
            .items
            .iter()
            .position(|((s, r), _, _)| s == session && r == resource)?;
        let entry = self.items.remove(index)?;
        let body = entry.1.clone();
        self.items.push_back(entry);
        Some(body)
    }
    pub fn set(&mut self, session: &str, resource: &str, text: String) {
        if let Some(index) = self
            .items
            .iter()
            .position(|((s, r), _, _)| s == session && r == resource)
        {
            if let Some((_, _, size)) = self.items.remove(index) {
                self.size -= size;
            }
        }
        let size = text.encode_utf16().count();
        self.size += size;
        self.items
            .push_back(((session.into(), resource.into()), text, size));
        while self.size > self.limit && self.items.len() > 1 {
            if let Some((_, _, size)) = self.items.pop_front() {
                self.size -= size;
            }
        }
    }
}
