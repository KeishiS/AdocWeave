use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyGraph<K: Ord> {
    forward: BTreeMap<K, BTreeSet<K>>,
    reverse: BTreeMap<K, BTreeSet<K>>,
}

impl<K: Ord> Default for DependencyGraph<K> {
    fn default() -> Self {
        Self {
            forward: BTreeMap::new(),
            reverse: BTreeMap::new(),
        }
    }
}

impl<K: Clone + Ord> DependencyGraph<K> {
    pub fn replace(&mut self, key: K, dependencies: BTreeSet<K>) {
        if let Some(previous) = self.forward.remove(&key) {
            for dependency in previous {
                remove_reverse(&mut self.reverse, &dependency, &key);
            }
        }
        for dependency in &dependencies {
            self.reverse
                .entry(dependency.clone())
                .or_default()
                .insert(key.clone());
        }
        self.forward.insert(key, dependencies);
    }

    pub fn remove(&mut self, key: &K) {
        if let Some(previous) = self.forward.remove(key) {
            for dependency in previous {
                remove_reverse(&mut self.reverse, &dependency, key);
            }
        }
    }

    pub fn affected(&self, key: &K) -> BTreeSet<K> {
        closure(key.clone(), |item| self.reverse.get(item))
    }

    pub fn dependencies(&self, key: &K) -> BTreeSet<K> {
        let mut output = closure(key.clone(), |item| self.forward.get(item));
        output.remove(key);
        output
    }
}

fn closure<'a, K: Clone + Ord + 'a>(
    root: K,
    edges: impl Fn(&K) -> Option<&'a BTreeSet<K>>,
) -> BTreeSet<K> {
    let mut found = BTreeSet::from([root.clone()]);
    let mut pending = VecDeque::from([root]);
    while let Some(item) = pending.pop_front() {
        if let Some(next) = edges(&item) {
            for value in next {
                if found.insert(value.clone()) {
                    pending.push_back(value.clone());
                }
            }
        }
    }
    found
}

fn remove_reverse<K: Ord>(reverse: &mut BTreeMap<K, BTreeSet<K>>, dependency: &K, owner: &K) {
    let remove_entry = reverse.get_mut(dependency).is_some_and(|owners| {
        owners.remove(owner);
        owners.is_empty()
    });
    if remove_entry {
        reverse.remove(dependency);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_forward_and_reverse_closures_atomically() {
        let mut graph = DependencyGraph::default();
        graph.replace("a", BTreeSet::from(["b"]));
        graph.replace("b", BTreeSet::from(["c"]));
        assert_eq!(graph.dependencies(&"a"), BTreeSet::from(["b", "c"]));
        assert_eq!(graph.affected(&"c"), BTreeSet::from(["a", "b", "c"]));

        graph.replace("b", BTreeSet::new());
        assert_eq!(graph.dependencies(&"a"), BTreeSet::from(["b"]));
        assert_eq!(graph.affected(&"c"), BTreeSet::from(["c"]));
    }
}
