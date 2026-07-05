use crate::capabilities::{CapabilityDecision, CapabilityGrant, CapabilityRequest, Capabilities, GrantSet};

#[derive(Debug, Default)]
pub struct PermissionSet {
    allowed: GrantSet,
}

impl PermissionSet {
    pub fn new(required: &Vec<(String, String)>) -> Self {
        let mut set = GrantSet::default();
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

impl Capabilities for PermissionSet {
    fn check(&self, req: &CapabilityRequest) -> CapabilityDecision {
        match self.check(req.kind.as_str()) {
            Ok(()) => CapabilityDecision::Granted,
            Err(reason) => CapabilityDecision::Denied(reason),
        }
    }

    fn grant(&mut self, grant: CapabilityGrant) {
        self.allowed.insert(grant.kind);
    }

    fn granted(&self) -> &GrantSet {
        &self.allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_check_reports_granted_request() {
        let permissions = PermissionSet::new(&vec![("proc".into(), "exec".into())]);
        assert!(matches!(
            Capabilities::check(&permissions, &CapabilityRequest::new("proc.exec")),
            CapabilityDecision::Granted
        ));
    }

    #[test]
    fn capabilities_check_reports_denied_request() {
        let permissions = PermissionSet::new(&Vec::new());
        assert!(matches!(
            Capabilities::check(&permissions, &CapabilityRequest::new("proc.exec")),
            CapabilityDecision::Denied(_)
        ));
    }

    #[test]
    fn capabilities_grant_is_reflected_in_granted_set() {
        let mut permissions = PermissionSet::new(&Vec::new());
        permissions.grant(CapabilityGrant::new("time"));
        assert!(permissions.granted().contains("time"));
        assert!(matches!(
            Capabilities::check(&permissions, &CapabilityRequest::new("time")),
            CapabilityDecision::Granted
        ));
    }
}
