use std::collections::HashMap;

/// Something worked out off the main thread (a diagram, the colours of a code block), by what it
/// is made from.
pub enum Slot<V> {
    /// Being worked out; meanwhile what stood at its place before the file changed, if any.
    Pending(Option<V>),
    Done(V),
    Failed(String),
}

pub type Cache<K, V> = HashMap<K, Slot<V>>;

impl<V> Slot<V> {
    /// What to show now: the result, or what stands in for it.
    pub fn shown(&self) -> Option<&V> {
        match self {
            Slot::Done(v) | Slot::Pending(Some(v)) => Some(v),
            _ => None,
        }
    }
}
