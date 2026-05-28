use std::collections::HashSet;

#[derive(Debug)]
pub struct PermissionSet {
    allowed: HashSet<String>,
}

impl PermissionSet {
    pub fn new(required: &Vec<(String, String)>) -> Self {
        let mut set = HashSet::new();
        for (left, right) in required {
            set.insert(format!("{}.{}", left, right));
        }
        Self { allowed: set }
    }

    pub fn check(&self, permission: &str) -> Result<(), String> {
        if self.allowed.contains(permission) {
            Ok(())
        } else {
            Err(format!(
                "Permission '{}' was used but not declared in requires {{}} block.\nTry: requires {{ {} }}\n\nAdd it like:\n\nrequires {{\n  {}\n}}",
                permission,
                permission,
                permission
            ))
        }
    }

    pub fn extend(&mut self, required: &[(String, String)]) {
        for (left, right) in required {
            self.allowed.insert(format!("{}.{}", left, right));
        }
    }

    pub fn list(&self) -> Vec<String> {
        self.allowed.iter().cloned().collect()
    }
}
