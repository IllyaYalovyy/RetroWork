//! Dependency validation and activation order.
//!
//! [`resolve`] is a pure function over manifests so that every rejection rule
//! can be tested without building a platform. A module is rejected rather than
//! half-loaded: removing a module removes its dependents too, each with a
//! reason that names the cause.

use std::collections::{BTreeMap, BTreeSet};

use semver::Version;

use crate::{
    id::ModuleId,
    module::{ModuleManifest, RejectReason},
};

/// Outcome of [`resolve`]. Indices refer to the input slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    /// Accepted modules, dependencies before dependents. Among modules that
    /// are ready at the same time the order is by id, so it never depends on
    /// registration order.
    pub order: Vec<usize>,
    pub rejected: BTreeMap<usize, RejectReason>,
}

/// Validates `manifests` against each other and against platform API `api`.
pub fn resolve(manifests: &[ModuleManifest], api: &Version) -> Resolution {
    let mut rejected: BTreeMap<usize, RejectReason> = BTreeMap::new();

    // First registration of an id wins; later ones are reported, never merged.
    let mut by_id: BTreeMap<&ModuleId, usize> = BTreeMap::new();
    for (index, manifest) in manifests.iter().enumerate() {
        if by_id.contains_key(&manifest.id) {
            rejected.insert(index, RejectReason::DuplicateId(manifest.id.clone()));
        } else {
            by_id.insert(&manifest.id, index);
        }
    }

    // Problems visible from the manifest alone.
    for (&id, &index) in &by_id {
        let manifest = &manifests[index];
        debug_assert_eq!(&manifest.id, id);
        if !manifest.requires_api.matches(api) {
            rejected.insert(
                index,
                RejectReason::IncompatibleApi {
                    required: manifest.requires_api.clone(),
                    actual: api.clone(),
                },
            );
            continue;
        }
        for dependency in &manifest.dependencies {
            let reason = match by_id.get(&dependency.id) {
                None => RejectReason::MissingDependency(dependency.id.clone()),
                Some(&found) if !dependency.version.matches(&manifests[found].version) => {
                    RejectReason::DependencyVersionMismatch {
                        id: dependency.id.clone(),
                        required: dependency.version.clone(),
                        found: manifests[found].version.clone(),
                    }
                }
                Some(_) => continue,
            };
            rejected.insert(index, reason);
            break;
        }
    }

    // A rejected dependency takes its dependents with it.
    loop {
        let mut changed = false;
        for &index in by_id.values() {
            if rejected.contains_key(&index) {
                continue;
            }
            let unavailable = manifests[index]
                .dependencies
                .iter()
                .find(|dependency| rejected.contains_key(&by_id[&dependency.id]));
            if let Some(dependency) = unavailable {
                rejected.insert(
                    index,
                    RejectReason::DependencyUnavailable(dependency.id.clone()),
                );
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Kahn's algorithm over what is left; whatever cannot be ordered is in or
    // behind a cycle.
    let mut remaining: BTreeMap<&ModuleId, BTreeSet<&ModuleId>> = by_id
        .iter()
        .filter(|(_, index)| !rejected.contains_key(index))
        .map(|(&id, &index)| {
            let dependencies = manifests[index]
                .dependencies
                .iter()
                .map(|dependency| &dependency.id)
                .collect();
            (id, dependencies)
        })
        .collect();
    let mut order = Vec::with_capacity(remaining.len());
    while let Some(ready) = remaining
        .iter()
        .find(|(_, dependencies)| dependencies.is_empty())
        .map(|(&id, _)| id)
    {
        remaining.remove(ready);
        for dependencies in remaining.values_mut() {
            dependencies.remove(ready);
        }
        order.push(by_id[ready]);
    }

    let stuck: BTreeSet<&ModuleId> = remaining.keys().copied().collect();
    for &id in &stuck {
        let index = by_id[id];
        let reason = match find_cycle(id, manifests, &by_id, &stuck) {
            Some(cycle) => RejectReason::DependencyCycle(cycle),
            None => {
                let blocked_on = manifests[index]
                    .dependencies
                    .iter()
                    .find(|dependency| stuck.contains(&dependency.id))
                    .expect("a stuck module that is not on a cycle depends on a stuck module");
                RejectReason::DependencyUnavailable(blocked_on.id.clone())
            }
        };
        rejected.insert(index, reason);
    }

    Resolution { order, rejected }
}

/// Returns a dependency path from `start` back to itself, if one exists.
fn find_cycle(
    start: &ModuleId,
    manifests: &[ModuleManifest],
    by_id: &BTreeMap<&ModuleId, usize>,
    stuck: &BTreeSet<&ModuleId>,
) -> Option<Vec<ModuleId>> {
    fn visit<'a>(
        current: &'a ModuleId,
        start: &ModuleId,
        manifests: &'a [ModuleManifest],
        by_id: &BTreeMap<&ModuleId, usize>,
        stuck: &BTreeSet<&ModuleId>,
        visited: &mut BTreeSet<&'a ModuleId>,
        path: &mut Vec<ModuleId>,
    ) -> bool {
        path.push(current.clone());
        for dependency in &manifests[by_id[current]].dependencies {
            if &dependency.id == start {
                path.push(start.clone());
                return true;
            }
            if stuck.contains(&dependency.id)
                && visited.insert(&dependency.id)
                && visit(
                    &dependency.id,
                    start,
                    manifests,
                    by_id,
                    stuck,
                    visited,
                    path,
                )
            {
                return true;
            }
        }
        path.pop();
        false
    }

    let mut path = Vec::new();
    let mut visited = BTreeSet::new();
    let start_ref = &manifests[by_id[start]].id;
    visited.insert(start_ref);
    visit(
        start_ref,
        start,
        manifests,
        by_id,
        stuck,
        &mut visited,
        &mut path,
    )
    .then_some(path)
}

#[cfg(test)]
mod tests {
    use semver::VersionReq;

    use super::*;
    use crate::module::{Dependency, ModuleKind, PLATFORM_API_VERSION};

    fn manifest(id: &str, version: &str, dependencies: &[(&str, &str)]) -> ModuleManifest {
        ModuleManifest {
            id: ModuleId::new(id).unwrap(),
            name: id.to_owned(),
            version: Version::parse(version).unwrap(),
            kind: ModuleKind::Bundled,
            requires_api: VersionReq::parse("^0.1").unwrap(),
            dependencies: dependencies
                .iter()
                .map(|(id, requirement)| Dependency {
                    id: ModuleId::new(id).unwrap(),
                    version: VersionReq::parse(requirement).unwrap(),
                })
                .collect(),
            extensions: Vec::new(),
        }
    }

    fn id(value: &str) -> ModuleId {
        ModuleId::new(value).unwrap()
    }

    fn accepted(manifests: &[ModuleManifest], resolution: &Resolution) -> Vec<String> {
        resolution
            .order
            .iter()
            .map(|&index| manifests[index].id.to_string())
            .collect()
    }

    fn reason<'a>(
        manifests: &[ModuleManifest],
        resolution: &'a Resolution,
        module: &str,
    ) -> &'a RejectReason {
        let index = manifests
            .iter()
            .position(|manifest| manifest.id.as_str() == module)
            .unwrap();
        resolution
            .rejected
            .get(&index)
            .unwrap_or_else(|| panic!("{module} was not rejected"))
    }

    #[test]
    fn orders_dependencies_before_dependents_regardless_of_registration_order() {
        let manifests = [
            manifest("stats", "1.0.0", &[("logs", "^1"), ("output", "^1")]),
            manifest("logs", "1.2.0", &[("output", "^1")]),
            manifest("output", "1.0.0", &[]),
            manifest("about", "1.0.0", &[]),
        ];
        let resolution = resolve(&manifests, &PLATFORM_API_VERSION);
        assert!(resolution.rejected.is_empty());
        assert_eq!(
            accepted(&manifests, &resolution),
            ["about", "output", "logs", "stats"]
        );
    }

    #[test]
    fn missing_dependency_rejects_the_whole_dependent_closure() {
        let manifests = [
            manifest("stats", "1.0.0", &[("logs", "^1")]),
            manifest("stats-export", "1.0.0", &[("stats", "^1")]),
            manifest("output", "1.0.0", &[]),
        ];
        let resolution = resolve(&manifests, &PLATFORM_API_VERSION);
        assert_eq!(accepted(&manifests, &resolution), ["output"]);
        assert_eq!(
            reason(&manifests, &resolution, "stats"),
            &RejectReason::MissingDependency(id("logs"))
        );
        assert_eq!(
            reason(&manifests, &resolution, "stats-export"),
            &RejectReason::DependencyUnavailable(id("stats"))
        );
    }

    #[test]
    fn version_mismatch_names_required_and_found_versions() {
        let manifests = [
            manifest("logs", "0.9.0", &[]),
            manifest("stats", "1.0.0", &[("logs", "^1.2")]),
        ];
        let resolution = resolve(&manifests, &PLATFORM_API_VERSION);
        assert_eq!(accepted(&manifests, &resolution), ["logs"]);
        assert_eq!(
            reason(&manifests, &resolution, "stats"),
            &RejectReason::DependencyVersionMismatch {
                id: id("logs"),
                required: VersionReq::parse("^1.2").unwrap(),
                found: Version::new(0, 9, 0),
            }
        );
    }

    #[test]
    fn incompatible_platform_api_is_rejected() {
        let mut future = manifest("future", "1.0.0", &[]);
        future.requires_api = VersionReq::parse("^0.2").unwrap();
        let manifests = [future, manifest("uses-future", "1.0.0", &[("future", "*")])];
        let resolution = resolve(&manifests, &PLATFORM_API_VERSION);
        assert!(resolution.order.is_empty());
        assert_eq!(
            reason(&manifests, &resolution, "future"),
            &RejectReason::IncompatibleApi {
                required: VersionReq::parse("^0.2").unwrap(),
                actual: PLATFORM_API_VERSION,
            }
        );
        assert_eq!(
            reason(&manifests, &resolution, "uses-future"),
            &RejectReason::DependencyUnavailable(id("future"))
        );
    }

    #[test]
    fn duplicate_id_keeps_the_first_registration_only() {
        let manifests = [
            manifest("logs", "1.0.0", &[]),
            manifest("logs", "2.0.0", &[]),
            manifest("stats", "1.0.0", &[("logs", "^1")]),
        ];
        let resolution = resolve(&manifests, &PLATFORM_API_VERSION);
        assert_eq!(resolution.order, [0, 2]);
        assert_eq!(
            resolution.rejected,
            BTreeMap::from([(1, RejectReason::DuplicateId(id("logs")))])
        );
    }

    #[test]
    fn cycles_are_reported_with_their_path_and_do_not_block_unrelated_modules() {
        let manifests = [
            manifest("a", "1.0.0", &[("b", "*")]),
            manifest("b", "1.0.0", &[("c", "*")]),
            manifest("c", "1.0.0", &[("a", "*")]),
            manifest("behind", "1.0.0", &[("a", "*")]),
            manifest("selfish", "1.0.0", &[("selfish", "*")]),
            manifest("free", "1.0.0", &[]),
        ];
        let resolution = resolve(&manifests, &PLATFORM_API_VERSION);
        assert_eq!(accepted(&manifests, &resolution), ["free"]);
        assert_eq!(
            reason(&manifests, &resolution, "a"),
            &RejectReason::DependencyCycle(vec![id("a"), id("b"), id("c"), id("a")])
        );
        assert_eq!(
            reason(&manifests, &resolution, "c"),
            &RejectReason::DependencyCycle(vec![id("c"), id("a"), id("b"), id("c")])
        );
        assert_eq!(
            reason(&manifests, &resolution, "selfish"),
            &RejectReason::DependencyCycle(vec![id("selfish"), id("selfish")])
        );
        assert_eq!(
            reason(&manifests, &resolution, "behind"),
            &RejectReason::DependencyUnavailable(id("a"))
        );
    }

    /// Small deterministic generator so the property below needs no extra crate.
    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }
    }

    /// Property: for unique ids, the verdict for every module and the
    /// activation order are functions of the manifest *set*, not of the order
    /// modules were registered in; and accepted modules always follow their
    /// dependencies.
    #[test]
    fn resolution_is_independent_of_registration_order() {
        for seed in 1..=200u64 {
            let mut random = XorShift(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let count = 2 + random.below(7);
            let names: Vec<String> = (0..count).map(|n| format!("m{n}")).collect();
            let mut manifests: Vec<ModuleManifest> = names
                .iter()
                .map(|name| {
                    let dependency_count = random.below(3);
                    let dependencies: Vec<(String, &str)> = (0..dependency_count)
                        .map(|_| {
                            // Sometimes point at a module that does not exist
                            // or at a version that is not there.
                            let target = match random.below(10) {
                                0 => "absent".to_owned(),
                                _ => names[random.below(count)].clone(),
                            };
                            let requirement = if random.below(8) == 0 { "^2" } else { "^1" };
                            (target, requirement)
                        })
                        .collect();
                    let dependencies: Vec<(&str, &str)> = dependencies
                        .iter()
                        .map(|(target, requirement)| (target.as_str(), *requirement))
                        .collect();
                    manifest(name, "1.0.0", &dependencies)
                })
                .collect();

            let verdict = |manifests: &[ModuleManifest]| {
                let resolution = resolve(manifests, &PLATFORM_API_VERSION);
                let position: BTreeMap<&ModuleId, usize> = resolution
                    .order
                    .iter()
                    .enumerate()
                    .map(|(position, &index)| (&manifests[index].id, position))
                    .collect();
                for &index in &resolution.order {
                    for dependency in &manifests[index].dependencies {
                        assert!(
                            position[&dependency.id] < position[&manifests[index].id],
                            "seed {seed}: {} activated before its dependency {}",
                            manifests[index].id,
                            dependency.id
                        );
                    }
                }
                assert_eq!(
                    resolution.order.len() + resolution.rejected.len(),
                    manifests.len(),
                    "seed {seed}: every module is either accepted or rejected"
                );
                let order: Vec<ModuleId> = resolution
                    .order
                    .iter()
                    .map(|&index| manifests[index].id.clone())
                    .collect();
                let rejected: BTreeMap<ModuleId, RejectReason> = resolution
                    .rejected
                    .iter()
                    .map(|(&index, reason)| (manifests[index].id.clone(), reason.clone()))
                    .collect();
                (order, rejected)
            };

            let expected = verdict(&manifests);
            for _ in 0..4 {
                for i in (1..manifests.len()).rev() {
                    manifests.swap(i, random.below(i + 1));
                }
                assert_eq!(verdict(&manifests), expected, "seed {seed}");
            }
        }
    }
}
