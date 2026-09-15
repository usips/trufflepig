use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RequestClass {
    Query,
    Background,
}

pub struct ScheduledRequest<T> {
    pub class: RequestClass,
    pub root_id: String,
    pub request: T,
}

struct RootQueue<T> {
    query: VecDeque<T>,
    background: VecDeque<T>,
}

pub struct FairScheduler<T> {
    roots: BTreeMap<String, RootQueue<T>>,
    order: Vec<String>,
    cursor: usize,
    query_turns: u8,
}

impl<T> Default for FairScheduler<T> {
    fn default() -> Self {
        Self {
            roots: BTreeMap::new(),
            order: Vec::new(),
            cursor: 0,
            query_turns: 0,
        }
    }
}

impl<T> FairScheduler<T> {
    pub fn push(&mut self, class: RequestClass, item: T)
    where
        T: RootIdent,
    {
        let root_id = item.root_id().to_owned();
        let queue = self.roots.entry(root_id.clone()).or_insert_with(|| {
            self.order.push(root_id.clone());
            RootQueue {
                query: VecDeque::new(),
                background: VecDeque::new(),
            }
        });
        match class {
            RequestClass::Query => queue.query.push_back(item),
            RequestClass::Background => queue.background.push_back(item),
        }
    }

    pub fn enqueue(&mut self, root_id: impl Into<String>, class: RequestClass, item: T) {
        let root_id = root_id.into();
        let queue = self.roots.entry(root_id.clone()).or_insert_with(|| {
            self.order.push(root_id.clone());
            RootQueue {
                query: VecDeque::new(),
                background: VecDeque::new(),
            }
        });
        match class {
            RequestClass::Query => queue.query.push_back(item),
            RequestClass::Background => queue.background.push_back(item),
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        if self.order.is_empty() {
            return None;
        }
        let preferred = if self.query_turns < 3 {
            RequestClass::Query
        } else {
            RequestClass::Background
        };
        let fallback = match preferred {
            RequestClass::Query => RequestClass::Background,
            RequestClass::Background => RequestClass::Query,
        };
        let mut selected = None;
        for class in [preferred, fallback] {
            for offset in 0..self.order.len() {
                let index = (self.cursor + offset) % self.order.len();
                let root = &self.order[index];
                let Some(queue) = self.roots.get_mut(root) else {
                    continue;
                };
                let item = match class {
                    RequestClass::Query => queue.query.pop_front(),
                    RequestClass::Background => queue.background.pop_front(),
                };
                if let Some(item) = item {
                    selected = Some((item, class, index));
                    break;
                }
            }
            if selected.is_some() {
                break;
            }
        }
        let Some((item, class, index)) = selected else {
            return None;
        };
        if class == RequestClass::Query {
            self.query_turns = self.query_turns.saturating_add(1);
        } else {
            self.query_turns = 0;
        }
        self.cursor = (index + 1) % self.order.len();
        self.remove_empty_roots();
        return Some(item);
    }

    pub fn pending(&self) -> usize {
        self.roots
            .values()
            .map(|queue| queue.query.len() + queue.background.len())
            .sum()
    }

    fn remove_empty_roots(&mut self) {
        let empty = self
            .roots
            .iter()
            .filter_map(|(root, queue)| {
                (queue.query.is_empty() && queue.background.is_empty()).then_some(root.clone())
            })
            .collect::<Vec<_>>();
        for root in empty {
            self.roots.remove(&root);
            if let Some(index) = self.order.iter().position(|entry| entry == &root) {
                self.order.remove(index);
                if self.order.is_empty() {
                    self.cursor = 0;
                } else if index < self.cursor {
                    self.cursor -= 1;
                } else {
                    self.cursor %= self.order.len();
                }
            }
        }
    }
}

pub trait RootIdent {
    fn root_id(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Item {
        root: String,
        id: usize,
    }
    impl RootIdent for Item {
        fn root_id(&self) -> &str {
            &self.root
        }
    }

    #[test]
    fn scheduler_keeps_background_progress_after_three_queries() {
        let mut scheduler = FairScheduler::default();
        for id in 0..8 {
            scheduler.push(
                RequestClass::Query,
                Item {
                    root: "a".into(),
                    id,
                },
            );
        }
        scheduler.push(
            RequestClass::Background,
            Item {
                root: "b".into(),
                id: 99,
            },
        );
        assert_eq!(scheduler.pop().unwrap().root, "a");
        assert_eq!(scheduler.pop().unwrap().root, "a");
        assert_eq!(scheduler.pop().unwrap().root, "a");
        assert_eq!(scheduler.pop().unwrap().root, "b");
    }

    #[test]
    fn scheduler_allows_three_queries_before_background_when_root_is_shared() {
        let mut scheduler = FairScheduler::default();
        for id in 0..3 {
            scheduler.push(
                RequestClass::Query,
                Item {
                    root: "a".into(),
                    id,
                },
            );
        }
        scheduler.push(
            RequestClass::Background,
            Item {
                root: "a".into(),
                id: 3,
            },
        );
        assert_eq!(scheduler.pop().unwrap().id, 0);
        assert_eq!(scheduler.pop().unwrap().id, 1);
        assert_eq!(scheduler.pop().unwrap().id, 2);
        assert_eq!(scheduler.pop().unwrap().id, 3);
    }
}
