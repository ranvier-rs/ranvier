use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize};

/// Represents a role in the system.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Role(pub String);

impl Role {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }
}

/// A DAG representing role inheritance (e.g., admin > manager > user).
#[derive(Debug, Default, Clone)]
pub struct RoleHierarchy {
    /// Maps a role to the set of roles it inherits from (i.e. roles that are considered "lesser" or included).
    edges: HashMap<Role, HashSet<Role>>,
}

impl RoleHierarchy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an inheritance rule: `parent` includes all permissions of `child`.
    pub fn inherit(&mut self, parent: Role, child: Role) {
        self.edges.entry(parent).or_default().insert(child);
    }

    /// Checks if a given `user_role` grants access to an `action_role` requirement.
    /// E.g. `has_role(admin, user)` returns true if admin inherits user.
    pub fn has_role(&self, user_role: &Role, required_role: &Role) -> bool {
        if user_role == required_role {
            return true;
        }

        // BFS/DFS to check inheritance
        let mut visited = HashSet::new();
        let mut stack = vec![user_role.clone()];

        while let Some(current) = stack.pop() {
            if &current == required_role {
                return true;
            }
            if visited.insert(current.clone()) {
                if let Some(children) = self.edges.get(&current) {
                    stack.extend(children.iter().cloned());
                }
            }
        }

        false
    }
}

/// Attribute-Based Access Control Context
pub trait AbacContext {
    fn get_attribute(&self, key: &str) -> Option<&str>;
}

/// A policy evaluator for ABAC rules.
pub trait AbacPolicy: Send + Sync {
    fn evaluate(&self, cx: &dyn AbacContext) -> bool;
}

/// Evaluator that requires all nested policies to be true.
pub struct AllOf(pub Vec<Box<dyn AbacPolicy>>);

impl AbacPolicy for AllOf {
    fn evaluate(&self, cx: &dyn AbacContext) -> bool {
        self.0.iter().all(|p| p.evaluate(cx))
    }
}

/// Evaluator that requires at least one nested policy to be true.
pub struct AnyOf(pub Vec<Box<dyn AbacPolicy>>);

impl AbacPolicy for AnyOf {
    fn evaluate(&self, cx: &dyn AbacContext) -> bool {
        self.0.iter().any(|p| p.evaluate(cx))
    }
}

/// A policy that checks if a specific attribute matches an exact string.
pub struct AttributeEquals {
    pub key: String,
    pub expected: String,
}

impl AbacPolicy for AttributeEquals {
    fn evaluate(&self, cx: &dyn AbacContext) -> bool {
        cx.get_attribute(&self.key) == Some(&self.expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_hierarchy() {
        let mut rh = RoleHierarchy::new();
        let admin = Role::new("admin");
        let manager = Role::new("manager");
        let user = Role::new("user");
        let guest = Role::new("guest");

        rh.inherit(admin.clone(), manager.clone());
        rh.inherit(manager.clone(), user.clone());
        rh.inherit(user.clone(), guest.clone());

        assert!(rh.has_role(&admin, &user)); // admin > manager > user
        assert!(rh.has_role(&manager, &guest)); // manager > user > guest
        assert!(!rh.has_role(&user, &admin)); // user !> admin
        assert!(!rh.has_role(&guest, &manager)); // guest !> manager
    }

    struct MockAbacContext(HashMap<String, String>);
    
    impl AbacContext for MockAbacContext {
        fn get_attribute(&self, key: &str) -> Option<&str> {
            self.0.get(key).map(|s| s.as_str())
        }
    }

    #[test]
    fn test_abac_policy() {
        let policy = AllOf(vec![
            Box::new(AttributeEquals {
                key: "department".into(),
                expected: "finance".into(),
            }),
            Box::new(AnyOf(vec![
                Box::new(AttributeEquals {
                    key: "clearance".into(),
                    expected: "level_2".into(),
                }),
                Box::new(AttributeEquals {
                    key: "clearance".into(),
                    expected: "top_secret".into(),
                }),
            ])),
        ]);

        let mut cx = HashMap::new();
        cx.insert("department".into(), "finance".into());
        cx.insert("clearance".into(), "level_2".into());
        
        assert!(policy.evaluate(&MockAbacContext(cx)));

        let mut cx_bad = HashMap::new();
        cx_bad.insert("department".into(), "hr".into());
        cx_bad.insert("clearance".into(), "level_2".into());
        
        assert!(!policy.evaluate(&MockAbacContext(cx_bad)));
    }
}
