use std::collections::{HashMap, HashSet};

/// A BLoader-owned native Mod. LeviLamina's own versioned manifest
/// dependencies are deliberately delegated to LeviLamina's registrar.
#[derive(Clone, Debug)]
pub(super) struct ModNode {
    pub id: String,
    pub dependencies: Vec<String>,
}

pub(super) struct LoadPlan {
    pub order: Vec<usize>,
    pub rejected: Vec<(usize, String)>,
}

/// Deterministic depth-first topological planning. A rejected provider blocks
/// its dependents; missing IDs, duplicate IDs and cycles are hard failures.
pub(super) fn plan_mods(nodes: &[ModNode], already_ready: &HashSet<String>) -> LoadPlan {
    let mut by_id = HashMap::<String, usize>::new();
    let mut duplicates = HashSet::<String>::new();
    for (index, node) in nodes.iter().enumerate() {
        let id = node.id.trim().to_ascii_lowercase();
        if by_id.insert(id.clone(), index).is_some() {
            duplicates.insert(id);
        }
    }

    fn visit(
        index: usize,
        nodes: &[ModNode],
        by_id: &HashMap<String, usize>,
        duplicates: &HashSet<String>,
        already_ready: &HashSet<String>,
        states: &mut [u8],
        errors: &mut [Option<String>],
        order: &mut Vec<usize>,
    ) -> Result<(), String> {
        match states[index] {
            1 => return Err(format!("dependency cycle involving {}", nodes[index].id)),
            2 => return Ok(()),
            3 => return Err(errors[index].clone().unwrap_or_else(|| "dependency unavailable".into())),
            _ => {}
        }

        let id = nodes[index].id.trim().to_ascii_lowercase();
        if id.is_empty() || duplicates.contains(&id) {
            let reason = format!("empty or duplicated Mod ID: {}", nodes[index].id);
            states[index] = 3;
            errors[index] = Some(reason.clone());
            return Err(reason);
        }

        states[index] = 1;
        for dependency in &nodes[index].dependencies {
            let required = dependency.trim().to_ascii_lowercase();
            if already_ready.contains(&required) {
                continue;
            }
            let result = match by_id.get(&required) {
                Some(&target) => visit(
                    target, nodes, by_id, duplicates, already_ready,
                    states, errors, order,
                ),
                None => Err(format!("missing dependency {dependency}")),
            };
            if let Err(error) = result {
                let reason = format!("{} requires {}: {error}", nodes[index].id, dependency);
                states[index] = 3;
                errors[index] = Some(reason.clone());
                return Err(reason);
            }
        }
        states[index] = 2;
        order.push(index);
        Ok(())
    }

    let mut states = vec![0u8; nodes.len()];
    let mut errors = vec![None; nodes.len()];
    let mut order = Vec::with_capacity(nodes.len());
    for index in 0..nodes.len() {
        let _ = visit(
            index, nodes, &by_id, &duplicates, already_ready,
            &mut states, &mut errors, &mut order,
        );
    }
    let rejected = errors.into_iter().enumerate()
        .filter_map(|(index, reason)| reason.map(|reason| (index, reason)))
        .collect();
    LoadPlan { order, rejected }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, deps: &[&str]) -> ModNode {
        ModNode {
            id: id.into(),
            dependencies: deps.iter().map(|dep| (*dep).into()).collect(),
        }
    }

    #[test]
    fn orders_native_mods_after_their_runtime() {
        let mods = [node("ClientMod", &["BLRuntime"]), node("BLRuntime", &[])];
        let plan = plan_mods(&mods, &HashSet::new());
        assert_eq!(plan.order, [1, 0]);
        assert!(plan.rejected.is_empty());
    }

    #[test]
    fn supports_already_loaded_preloader() {
        let mods = [node("Addon", &["PreLoader"])];
        let ready = HashSet::from(["preloader".to_string()]);
        let plan = plan_mods(&mods, &ready);
        assert_eq!(plan.order, [0]);
        assert!(plan.rejected.is_empty());
    }

    #[test]
    fn missing_dependency_blocks_mod() {
        let plan = plan_mods(&[node("Addon", &["NotInstalled"])], &HashSet::new());
        assert!(plan.order.is_empty());
        assert_eq!(plan.rejected.len(), 1);
    }

    #[test]
    fn cycles_and_dependents_are_rejected() {
        let mods = [
            node("A", &["B"]),
            node("B", &["A"]),
            node("C", &["A"]),
        ];
        let plan = plan_mods(&mods, &HashSet::new());
        assert!(plan.order.is_empty());
        assert_eq!(plan.rejected.len(), 3);
    }
}
