//! Minimal directed acyclic graph utilities used by the scheduler.

use std::collections::VecDeque;

use anyhow::{bail, Result};

/// A DAG over `0..n` where `deps[i]` are the nodes that must finish before `i`.
#[derive(Debug, Clone)]
pub struct Dag {
    pub n: usize,
    pub deps: Vec<Vec<usize>>,
    pub dependents: Vec<Vec<usize>>,
    pub indegree: Vec<usize>,
}

impl Dag {
    pub fn new(n: usize, deps: Vec<Vec<usize>>) -> Result<Self> {
        if deps.len() != n {
            bail!("internal error: dependency list length mismatch");
        }
        let mut dependents = vec![Vec::new(); n];
        let mut indegree = vec![0usize; n];
        for (node, node_deps) in deps.iter().enumerate() {
            let mut seen = std::collections::HashSet::new();
            for &dep in node_deps {
                if dep >= n {
                    bail!("internal error: step {node} depends on out-of-range step {dep}");
                }
                if dep == node {
                    bail!("internal error: step {node} depends on itself");
                }
                if !seen.insert(dep) {
                    continue;
                }
                dependents[dep].push(node);
                indegree[node] += 1;
            }
        }
        Ok(Self {
            n,
            deps,
            dependents,
            indegree,
        })
    }

    /// Kahn's algorithm. Returns the topological order or an error describing a
    /// cycle.
    pub fn topo_order(&self) -> Result<Vec<usize>> {
        let mut indegree = self.indegree.clone();
        let mut queue: VecDeque<usize> = (0..self.n).filter(|&i| indegree[i] == 0).collect();
        let mut order = Vec::with_capacity(self.n);
        while let Some(node) = queue.pop_front() {
            order.push(node);
            for &next in &self.dependents[node] {
                indegree[next] -= 1;
                if indegree[next] == 0 {
                    queue.push_back(next);
                }
            }
        }
        if order.len() != self.n {
            let stuck: Vec<usize> = (0..self.n).filter(|&i| indegree[i] > 0).collect();
            bail!(
                "dependency cycle detected between {} step(s): {:?}",
                stuck.len(),
                &stuck[..stuck.len().min(12)]
            );
        }
        Ok(order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topological_order_respects_dependencies() {
        let dag = Dag::new(4, vec![vec![], vec![0], vec![0, 1], vec![1]]).unwrap();
        let order = dag.topo_order().unwrap();
        let pos = |x: usize| order.iter().position(|&y| y == x).unwrap();
        assert!(pos(0) < pos(1));
        assert!(pos(1) < pos(2));
        assert!(pos(1) < pos(3));
    }

    #[test]
    fn detects_cycles() {
        let dag = Dag::new(3, vec![vec![2], vec![0], vec![1]]).unwrap();
        assert!(dag.topo_order().is_err());
    }

    #[test]
    fn deduplicates_edges() {
        let dag = Dag::new(2, vec![vec![], vec![0, 0]]).unwrap();
        assert_eq!(dag.indegree[1], 1);
    }
}
